//! Incremental bounded framing for text event streams.
//!
//! Complete frames are borrowed from one reusable byte buffer. Only an
//! incomplete tail survives between calls, UTF-8 is checked after a delimiter
//! is found, and a scan cursor prevents re-reading the retained prefix.

use std::borrow::Cow;
use std::fmt;

pub const DEFAULT_MAX_SSE_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_SSE_FRAME_BYTES_ENV: &str = "OPENPROXY_MAX_SSE_FRAME_BYTES";

const COMPACT_CAPACITY_THRESHOLD: usize = 64 * 1024;
const COMPACT_TAIL_RATIO: usize = 4;

pub fn max_sse_frame_bytes() -> usize {
    configured_max_sse_frame_bytes(std::env::var(MAX_SSE_FRAME_BYTES_ENV).ok().as_deref())
}

fn configured_max_sse_frame_bytes(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_SSE_FRAME_BYTES)
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("upstream SSE frame exceeded the {limit}-byte limit")]
    FrameTooLarge { limit: usize },
    #[error("upstream SSE frame buffer size overflow")]
    ArithmeticOverflow,
    #[error("unable to reserve upstream SSE frame buffer capacity")]
    Capacity,
    #[error("completed upstream SSE frame is not valid UTF-8")]
    InvalidUtf8,
}

impl FrameError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::FrameTooLarge { .. } => "upstream_sse_frame_too_large",
            Self::ArithmeticOverflow => "upstream_sse_frame_size_overflow",
            Self::Capacity => "upstream_sse_frame_capacity",
            Self::InvalidUtf8 => "upstream_sse_invalid_utf8",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameMode {
    Sse,
    Line,
}

#[derive(Clone)]
struct BoundedFramer {
    buffer: Vec<u8>,
    start: usize,
    scan: usize,
    line_start: usize,
    last_eol_start: Option<usize>,
    max_frame_bytes: usize,
    scanned_bytes: usize,
    mode: FrameMode,
}

impl fmt::Debug for BoundedFramer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedFramer")
            .field("pending_bytes", &self.pending_len())
            .field("buffer_capacity", &self.buffer.capacity())
            .field("scan", &self.scan)
            .field("max_frame_bytes", &self.max_frame_bytes)
            .field("scanned_bytes", &self.scanned_bytes)
            .field("mode", &self.mode)
            .finish()
    }
}

impl BoundedFramer {
    fn new(mode: FrameMode, max_frame_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
            scan: 0,
            line_start: 0,
            last_eol_start: None,
            max_frame_bytes: max_frame_bytes.max(1),
            scanned_bytes: 0,
            mode,
        }
    }

    fn pending_len(&self) -> usize {
        self.buffer.len().saturating_sub(self.start)
    }

    fn append(&mut self, chunk: &[u8]) -> Result<(), FrameError> {
        let next_len = self
            .buffer
            .len()
            .checked_add(chunk.len())
            .ok_or(FrameError::ArithmeticOverflow)?;
        if next_len > self.buffer.capacity() && self.start > 0 {
            self.compact_in_place();
        }
        self.buffer
            .try_reserve(chunk.len())
            .map_err(|_| FrameError::Capacity)?;
        self.buffer.extend_from_slice(chunk);
        Ok(())
    }

    fn feed<F>(&mut self, chunk: &[u8], mut on_frame: F) -> Result<(), FrameError>
    where
        F: FnMut(&[u8]) -> Result<(), FrameError>,
    {
        let framing_slack = match self.mode {
            FrameMode::Sse => 4,
            FrameMode::Line => 2,
        };
        let segment_limit = self
            .max_frame_bytes
            .checked_add(framing_slack)
            .ok_or(FrameError::ArithmeticOverflow)?;
        let mut offset = 0usize;
        while offset < chunk.len() {
            let available = segment_limit.saturating_sub(self.pending_len()).max(1);
            let end = offset
                .checked_add(available.min(chunk.len() - offset))
                .ok_or(FrameError::ArithmeticOverflow)?;
            self.append(&chunk[offset..end])?;
            offset = end;

            while let Some((frame_end, delimiter_end)) = self.next_boundary()? {
                let frame_len = frame_end.saturating_sub(self.start);
                if frame_len > self.max_frame_bytes {
                    return Err(FrameError::FrameTooLarge {
                        limit: self.max_frame_bytes,
                    });
                }
                on_frame(&self.buffer[self.start..frame_end])?;
                self.start = delimiter_end;
                self.scan = delimiter_end;
                self.line_start = delimiter_end;
                self.last_eol_start = None;
            }
            self.check_incomplete_limit()?;
            self.compact_large_capacity_for_tiny_tail()?;
        }
        Ok(())
    }

    fn finish<F>(&mut self, mut on_frame: F) -> Result<(), FrameError>
    where
        F: FnMut(&[u8]) -> Result<(), FrameError>,
    {
        if self.pending_len() > 0 {
            let frame_end = if self.mode == FrameMode::Sse && self.line_start == self.buffer.len() {
                self.last_eol_start.unwrap_or(self.buffer.len())
            } else {
                self.buffer.len()
            };
            let frame_len = frame_end.saturating_sub(self.start);
            if frame_len > self.max_frame_bytes {
                return Err(FrameError::FrameTooLarge {
                    limit: self.max_frame_bytes,
                });
            }
            on_frame(&self.buffer[self.start..frame_end])?;
        }
        self.reset();
        Ok(())
    }

    fn next_boundary(&mut self) -> Result<Option<(usize, usize)>, FrameError> {
        while self.scan < self.buffer.len() {
            let byte = self.buffer[self.scan];
            self.scanned_bytes = self
                .scanned_bytes
                .checked_add(1)
                .ok_or(FrameError::ArithmeticOverflow)?;
            if byte != b'\n' {
                self.scan += 1;
                continue;
            }

            let eol_start = if self.scan > self.line_start && self.buffer[self.scan - 1] == b'\r' {
                self.scan - 1
            } else {
                self.scan
            };
            let line_is_empty = eol_start == self.line_start;
            let delimiter = match self.mode {
                FrameMode::Line => Some((eol_start, self.scan + 1)),
                FrameMode::Sse if line_is_empty => {
                    Some((self.last_eol_start.unwrap_or(self.start), self.scan + 1))
                }
                FrameMode::Sse => None,
            };
            self.scan += 1;
            if let Some(delimiter) = delimiter {
                return Ok(Some(delimiter));
            }
            self.last_eol_start = Some(eol_start);
            self.line_start = self.scan;
        }
        Ok(None)
    }

    fn check_incomplete_limit(&self) -> Result<(), FrameError> {
        let pending_end = self.buffer.len();
        let pending = if self.mode == FrameMode::Sse
            && self.line_start + 1 == pending_end
            && self.buffer.get(self.line_start) == Some(&b'\r')
            && self.last_eol_start.is_some()
        {
            self.last_eol_start
                .unwrap_or(pending_end)
                .saturating_sub(self.start)
        } else if self.buffer.last() == Some(&b'\r') {
            self.pending_len().saturating_sub(1)
        } else if self.mode == FrameMode::Sse
            && (self.line_start == pending_end
                || (self.line_start + 1 == pending_end
                    && self.buffer.get(self.line_start) == Some(&b'\r')))
        {
            self.last_eol_start
                .unwrap_or(pending_end)
                .saturating_sub(self.start)
        } else {
            self.pending_len()
        };
        if pending > self.max_frame_bytes {
            return Err(FrameError::FrameTooLarge {
                limit: self.max_frame_bytes,
            });
        }
        Ok(())
    }

    fn compact_in_place(&mut self) {
        if self.start == 0 {
            return;
        }
        let old_start = self.start;
        let retained = self.pending_len();
        self.buffer.copy_within(old_start.., 0);
        self.buffer.truncate(retained);
        self.start = 0;
        self.scan = self.scan.saturating_sub(old_start);
        self.line_start = self.line_start.saturating_sub(old_start);
        self.last_eol_start = self
            .last_eol_start
            .map(|position| position.saturating_sub(old_start));
    }

    fn compact_large_capacity_for_tiny_tail(&mut self) -> Result<(), FrameError> {
        let retained = self.pending_len();
        let capacity = self.buffer.capacity();
        if capacity <= COMPACT_CAPACITY_THRESHOLD
            || retained.saturating_mul(COMPACT_TAIL_RATIO) >= capacity
        {
            return Ok(());
        }

        if retained == 0 {
            self.buffer = Vec::new();
            self.start = 0;
            self.scan = 0;
            self.line_start = 0;
            self.last_eol_start = None;
            return Ok(());
        }

        let old_start = self.start;
        let mut compact = Vec::new();
        compact
            .try_reserve(retained)
            .map_err(|_| FrameError::Capacity)?;
        compact.extend_from_slice(&self.buffer[old_start..]);
        self.buffer = compact;
        self.start = 0;
        self.scan = self.scan.saturating_sub(old_start);
        self.line_start = self.line_start.saturating_sub(old_start);
        self.last_eol_start = self
            .last_eol_start
            .map(|position| position.saturating_sub(old_start));
        Ok(())
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.start = 0;
        self.scan = 0;
        self.line_start = 0;
        self.last_eol_start = None;
        if self.buffer.capacity() > COMPACT_CAPACITY_THRESHOLD {
            self.buffer = Vec::new();
        }
    }
}

#[derive(Debug)]
pub struct SseEvent<'a> {
    raw: &'a str,
    data: Option<Cow<'a, str>>,
    event: Option<&'a str>,
    id: Option<&'a str>,
    retry: Option<u64>,
    comment_count: usize,
}

impl<'a> SseEvent<'a> {
    fn parse(frame: &'a [u8]) -> Result<Self, FrameError> {
        let raw = std::str::from_utf8(frame).map_err(|_| FrameError::InvalidUtf8)?;
        let mut data: Option<Cow<'a, str>> = None;
        let mut event = None;
        let mut id = None;
        let mut retry = None;
        let mut comment_count = 0usize;

        for mut line in raw.split('\n') {
            line = line.strip_suffix('\r').unwrap_or(line);
            if line.starts_with(':') {
                comment_count = comment_count
                    .checked_add(1)
                    .ok_or(FrameError::ArithmeticOverflow)?;
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "data" => match data.as_mut() {
                    None => data = Some(Cow::Borrowed(value)),
                    Some(existing) => {
                        let extra = value
                            .len()
                            .checked_add(1)
                            .ok_or(FrameError::ArithmeticOverflow)?;
                        let joined = existing.to_mut();
                        joined
                            .try_reserve(extra)
                            .map_err(|_| FrameError::Capacity)?;
                        joined.push('\n');
                        joined.push_str(value);
                    }
                },
                "event" => event = Some(value),
                "id" if !value.contains('\0') => id = Some(value),
                "retry" => retry = value.parse::<u64>().ok(),
                _ => {}
            }
        }

        Ok(Self {
            raw,
            data,
            event,
            id,
            retry,
            comment_count,
        })
    }

    pub fn raw(&self) -> &'a str {
        self.raw
    }

    pub fn data(&self) -> Option<&str> {
        self.data.as_deref()
    }

    pub fn event(&self) -> Option<&'a str> {
        self.event
    }

    pub fn id(&self) -> Option<&'a str> {
        self.id
    }

    pub fn retry(&self) -> Option<u64> {
        self.retry
    }

    pub fn comment_count(&self) -> usize {
        self.comment_count
    }
}

#[derive(Clone, Debug)]
pub struct SseFramer {
    inner: BoundedFramer,
}

impl Default for SseFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl SseFramer {
    pub fn new() -> Self {
        Self::with_max_frame_bytes(max_sse_frame_bytes())
    }

    pub fn with_max_frame_bytes(max_frame_bytes: usize) -> Self {
        Self {
            inner: BoundedFramer::new(FrameMode::Sse, max_frame_bytes),
        }
    }

    pub fn feed<F>(&mut self, chunk: &[u8], mut on_event: F) -> Result<(), FrameError>
    where
        F: FnMut(SseEvent<'_>),
    {
        self.inner.feed(chunk, |frame| {
            on_event(SseEvent::parse(frame)?);
            Ok(())
        })
    }

    /// At clean EOF, a non-empty final event is dispatched even without a
    /// trailing blank line. Empty tails are ignored.
    pub fn finish<F>(&mut self, mut on_event: F) -> Result<(), FrameError>
    where
        F: FnMut(SseEvent<'_>),
    {
        self.inner.finish(|frame| {
            if !frame.is_empty() {
                on_event(SseEvent::parse(frame)?);
            }
            Ok(())
        })
    }

    pub fn pending_len(&self) -> usize {
        self.inner.pending_len()
    }

    #[doc(hidden)]
    pub fn scanned_bytes(&self) -> usize {
        self.inner.scanned_bytes
    }

    #[doc(hidden)]
    pub fn buffer_capacity(&self) -> usize {
        self.inner.buffer.capacity()
    }
}

#[derive(Clone, Debug)]
pub struct LineFramer {
    inner: BoundedFramer,
}

/// Wire framing used by text streaming protocols. Binary Cursor and Kiro
/// streams intentionally do not use this abstraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStreamMode {
    Sse,
    Lines,
}

/// One complete source record. `payload()` returns the joined SSE `data`
/// fields or the JSON payload of a line/NDJSON record. CommandCode executors
/// may wrap NDJSON records in `data:` lines, so line mode accepts both forms.
#[derive(Debug)]
pub enum TextStreamFrame<'a> {
    Sse(SseEvent<'a>),
    Line(&'a str),
}

impl<'a> TextStreamFrame<'a> {
    pub fn raw(&self) -> &str {
        match self {
            Self::Sse(event) => event.raw(),
            Self::Line(line) => line,
        }
    }

    pub fn payload(&self) -> Option<&str> {
        match self {
            Self::Sse(event) => event.data(),
            Self::Line(line) => {
                let line = line.trim();
                if line.is_empty()
                    || line.starts_with(':')
                    || line.starts_with("event:")
                    || line.starts_with("id:")
                    || line.starts_with("retry:")
                {
                    return None;
                }
                Some(
                    line.strip_prefix("data:")
                        .map(str::trim_start)
                        .unwrap_or(line),
                )
            }
        }
    }

    pub fn event(&self) -> Option<&str> {
        match self {
            Self::Sse(event) => event.event(),
            Self::Line(_) => None,
        }
    }

    pub fn is_sse(&self) -> bool {
        matches!(self, Self::Sse(_))
    }
}

#[derive(Clone, Debug)]
pub enum TextStreamFramer {
    Sse(SseFramer),
    Lines(LineFramer),
}

impl TextStreamFramer {
    pub fn new(mode: TextStreamMode) -> Self {
        Self::with_max_frame_bytes(mode, max_sse_frame_bytes())
    }

    pub fn with_max_frame_bytes(mode: TextStreamMode, max_frame_bytes: usize) -> Self {
        match mode {
            TextStreamMode::Sse => Self::Sse(SseFramer::with_max_frame_bytes(max_frame_bytes)),
            TextStreamMode::Lines => Self::Lines(LineFramer::with_max_line_bytes(max_frame_bytes)),
        }
    }

    pub fn mode(&self) -> TextStreamMode {
        match self {
            Self::Sse(_) => TextStreamMode::Sse,
            Self::Lines(_) => TextStreamMode::Lines,
        }
    }

    pub fn feed<F>(&mut self, chunk: &[u8], mut on_frame: F) -> Result<(), FrameError>
    where
        F: FnMut(TextStreamFrame<'_>),
    {
        match self {
            Self::Sse(framer) => framer.feed(chunk, |event| on_frame(TextStreamFrame::Sse(event))),
            Self::Lines(framer) => framer.feed(chunk, |line| on_frame(TextStreamFrame::Line(line))),
        }
    }

    pub fn finish<F>(&mut self, mut on_frame: F) -> Result<(), FrameError>
    where
        F: FnMut(TextStreamFrame<'_>),
    {
        match self {
            Self::Sse(framer) => framer.finish(|event| on_frame(TextStreamFrame::Sse(event))),
            Self::Lines(framer) => framer.finish(|line| on_frame(TextStreamFrame::Line(line))),
        }
    }

    pub fn pending_len(&self) -> usize {
        match self {
            Self::Sse(framer) => framer.pending_len(),
            Self::Lines(framer) => framer.pending_len(),
        }
    }
}

impl Default for LineFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl LineFramer {
    pub fn new() -> Self {
        Self::with_max_line_bytes(max_sse_frame_bytes())
    }

    pub fn with_max_line_bytes(max_line_bytes: usize) -> Self {
        Self {
            inner: BoundedFramer::new(FrameMode::Line, max_line_bytes),
        }
    }

    pub fn feed<F>(&mut self, chunk: &[u8], mut on_line: F) -> Result<(), FrameError>
    where
        F: FnMut(&str),
    {
        self.inner.feed(chunk, |line| {
            let line = std::str::from_utf8(line).map_err(|_| FrameError::InvalidUtf8)?;
            on_line(line);
            Ok(())
        })
    }

    pub fn finish<F>(&mut self, mut on_line: F) -> Result<(), FrameError>
    where
        F: FnMut(&str),
    {
        self.inner.finish(|line| {
            if !line.is_empty() {
                let line = std::str::from_utf8(line).map_err(|_| FrameError::InvalidUtf8)?;
                on_line(line);
            }
            Ok(())
        })
    }

    pub fn pending_len(&self) -> usize {
        self.inner.pending_len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields_and_multiline_data() {
        let mut framer = SseFramer::with_max_frame_bytes(1024);
        let mut observed = None;
        framer
            .feed(
                b": heartbeat\r\nevent: update\r\nid: 7\r\nretry: 250\r\ndata: one\r\ndata: two\r\n\r\n",
                |event| {
                    observed = Some((
                        event.data().unwrap().to_string(),
                        event.event().unwrap().to_string(),
                        event.id().unwrap().to_string(),
                        event.retry().unwrap(),
                        event.comment_count(),
                    ));
                },
            )
            .unwrap();
        assert_eq!(
            observed,
            Some(("one\ntwo".into(), "update".into(), "7".into(), 250, 1))
        );
    }

    #[test]
    fn exact_limit_and_one_over() {
        let mut exact = SseFramer::with_max_frame_bytes(4);
        exact.feed(b"data\n\n", |_| {}).unwrap();

        let mut over = SseFramer::with_max_frame_bytes(4);
        assert!(matches!(
            over.feed(b"12345", |_| {}),
            Err(FrameError::FrameTooLarge { limit: 4 })
        ));
    }

    #[test]
    fn environment_override_must_be_a_positive_integer() {
        assert_eq!(configured_max_sse_frame_bytes(Some("4096")), 4096);
        for invalid in [None, Some(""), Some("0"), Some("-1"), Some("1MiB")] {
            assert_eq!(
                configured_max_sse_frame_bytes(invalid),
                DEFAULT_MAX_SSE_FRAME_BYTES
            );
        }
    }
}
