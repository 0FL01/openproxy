use std::fmt;

use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::BodyExt;
use reqwest::header::{HeaderMap, CONTENT_ENCODING, CONTENT_LENGTH};

use super::UpstreamResponse;

pub const DEFAULT_SUCCESS_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_DIAGNOSTIC_BODY_LIMIT_BYTES: usize = 1024 * 1024;
pub const SUCCESS_BODY_LIMIT_ENV: &str = "OPENPROXY_MAX_UPSTREAM_JSON_BYTES";
pub const DIAGNOSTIC_BODY_LIMIT_ENV: &str = "OPENPROXY_MAX_UPSTREAM_DIAGNOSTIC_BYTES";
pub const DIAGNOSTIC_TRUNCATION_MARKER: &[u8] =
    b"\n[openproxy: upstream diagnostic body truncated]\n";
pub const DIAGNOSTIC_TRANSPORT_MARKER: &[u8] =
    b"\n[openproxy: upstream diagnostic body incomplete after transport failure]\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedBodyError {
    DeclaredTooLarge { declared: u64, limit: usize },
    TooLarge { limit: usize },
    Capacity { limit: usize },
    Transport(String),
}

impl fmt::Display for BoundedBodyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeclaredTooLarge { declared, limit } => write!(
                formatter,
                "upstream response declared {declared} bytes, exceeding the {limit}-byte limit"
            ),
            Self::TooLarge { limit } => {
                write!(
                    formatter,
                    "upstream response exceeded the {limit}-byte limit"
                )
            }
            Self::Capacity { limit } => write!(
                formatter,
                "unable to allocate within the {limit}-byte upstream response limit"
            ),
            Self::Transport(message) => {
                write!(formatter, "upstream response body read failed: {message}")
            }
        }
    }
}

impl std::error::Error for BoundedBodyError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticBody {
    pub bytes: Bytes,
    pub truncated: bool,
    pub transport_failed: bool,
}

pub fn success_body_limit() -> usize {
    configured_limit(SUCCESS_BODY_LIMIT_ENV, DEFAULT_SUCCESS_BODY_LIMIT_BYTES)
}

pub fn diagnostic_body_limit() -> usize {
    configured_limit(
        DIAGNOSTIC_BODY_LIMIT_ENV,
        DEFAULT_DIAGNOSTIC_BODY_LIMIT_BYTES,
    )
}

fn configured_limit(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn declared_length_is_identity_encoded(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.trim().is_empty() || value.eq_ignore_ascii_case("identity"))
}

#[derive(Debug)]
struct BoundedAccumulator {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedAccumulator {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn push_strict(&mut self, chunk: &[u8]) -> Result<(), BoundedBodyError> {
        let next_len = self
            .bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(BoundedBodyError::TooLarge { limit: self.limit })?;
        if next_len > self.limit {
            return Err(BoundedBodyError::TooLarge { limit: self.limit });
        }
        self.bytes
            .try_reserve(chunk.len())
            .map_err(|_| BoundedBodyError::Capacity { limit: self.limit })?;
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    fn push_prefix(&mut self, chunk: &[u8]) -> Result<bool, BoundedBodyError> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        let take = remaining.min(chunk.len());
        let next_len = self
            .bytes
            .len()
            .checked_add(take)
            .ok_or(BoundedBodyError::TooLarge { limit: self.limit })?;
        if next_len > self.limit {
            return Err(BoundedBodyError::TooLarge { limit: self.limit });
        }
        self.bytes
            .try_reserve(take)
            .map_err(|_| BoundedBodyError::Capacity { limit: self.limit })?;
        self.bytes.extend_from_slice(&chunk[..take]);
        Ok(take != chunk.len())
    }

    fn finish(self) -> Bytes {
        Bytes::from(self.bytes)
    }

    fn finish_marked(mut self, marker: &[u8]) -> Result<Bytes, BoundedBodyError> {
        if marker.len() >= self.limit {
            self.bytes.clear();
            let marker_len = 0usize
                .checked_add(self.limit)
                .ok_or(BoundedBodyError::TooLarge { limit: self.limit })?;
            self.bytes
                .try_reserve(marker_len)
                .map_err(|_| BoundedBodyError::Capacity { limit: self.limit })?;
            self.bytes.extend_from_slice(&marker[..self.limit]);
            return Ok(Bytes::from(self.bytes));
        }

        self.bytes.truncate(self.limit - marker.len());
        let next_len = self
            .bytes
            .len()
            .checked_add(marker.len())
            .ok_or(BoundedBodyError::TooLarge { limit: self.limit })?;
        debug_assert!(next_len <= self.limit);
        self.bytes
            .try_reserve(marker.len())
            .map_err(|_| BoundedBodyError::Capacity { limit: self.limit })?;
        self.bytes.extend_from_slice(marker);
        Ok(Bytes::from(self.bytes))
    }
}

fn marked_or_empty(collected: BoundedAccumulator, marker: &[u8]) -> Bytes {
    collected.finish_marked(marker).unwrap_or_default()
}

pub async fn read_upstream_body(
    response: UpstreamResponse,
    limit: usize,
) -> Result<Bytes, BoundedBodyError> {
    match response {
        UpstreamResponse::Reqwest(response) => read_reqwest_body(response, limit).await,
        UpstreamResponse::Hyper(response) => {
            if declared_length_is_identity_encoded(response.headers()) {
                if let Some(declared) = declared_length(response.headers()) {
                    reject_declared_length(declared, limit)?;
                }
            }
            let mut body = response.into_body();
            let mut collected = BoundedAccumulator::new(limit);
            while let Some(frame) = body.frame().await {
                let frame =
                    frame.map_err(|error| BoundedBodyError::Transport(error.to_string()))?;
                if let Ok(data) = frame.into_data() {
                    collected.push_strict(&data)?;
                }
            }
            Ok(collected.finish())
        }
    }
}

pub async fn read_reqwest_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, BoundedBodyError> {
    // Reqwest transparently decompresses supported content encodings. A
    // compressed wire Content-Length therefore cannot prove that the decoded
    // body exceeds this decoded-byte budget; the per-chunk checks below remain
    // authoritative for every response.
    if declared_length_is_identity_encoded(response.headers()) {
        if let Some(declared) = declared_length(response.headers()) {
            reject_declared_length(declared, limit)?;
        }
    }
    let mut stream = response.bytes_stream();
    let mut collected = BoundedAccumulator::new(limit);
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|error| BoundedBodyError::Transport(error.to_string()))?
    {
        collected.push_strict(&chunk)?;
    }
    Ok(collected.finish())
}

fn reject_declared_length(declared: u64, limit: usize) -> Result<(), BoundedBodyError> {
    if declared > limit as u64 {
        Err(BoundedBodyError::DeclaredTooLarge { declared, limit })
    } else {
        Ok(())
    }
}

pub async fn read_upstream_diagnostic(response: UpstreamResponse, limit: usize) -> DiagnosticBody {
    match response {
        UpstreamResponse::Reqwest(response) => read_reqwest_diagnostic(response, limit).await,
        UpstreamResponse::Hyper(response) => {
            let mut body = response.into_body();
            let mut collected = BoundedAccumulator::new(limit);
            while let Some(frame) = body.frame().await {
                match frame {
                    Ok(frame) => {
                        if let Ok(data) = frame.into_data() {
                            match collected.push_prefix(&data) {
                                Ok(true) | Err(_) => {
                                    return DiagnosticBody {
                                        bytes: marked_or_empty(
                                            collected,
                                            DIAGNOSTIC_TRUNCATION_MARKER,
                                        ),
                                        truncated: true,
                                        transport_failed: false,
                                    };
                                }
                                Ok(false) => {}
                            }
                        }
                    }
                    Err(_) => {
                        return DiagnosticBody {
                            bytes: marked_or_empty(collected, DIAGNOSTIC_TRANSPORT_MARKER),
                            truncated: false,
                            transport_failed: true,
                        };
                    }
                }
            }
            DiagnosticBody {
                bytes: collected.finish(),
                truncated: false,
                transport_failed: false,
            }
        }
    }
}

pub async fn read_reqwest_diagnostic(response: reqwest::Response, limit: usize) -> DiagnosticBody {
    let mut stream = response.bytes_stream();
    let mut collected = BoundedAccumulator::new(limit);
    loop {
        match stream.try_next().await {
            Ok(Some(chunk)) => match collected.push_prefix(&chunk) {
                Ok(true) | Err(_) => {
                    return DiagnosticBody {
                        bytes: marked_or_empty(collected, DIAGNOSTIC_TRUNCATION_MARKER),
                        truncated: true,
                        transport_failed: false,
                    };
                }
                Ok(false) => {}
            },
            Ok(None) => {
                return DiagnosticBody {
                    bytes: collected.finish(),
                    truncated: false,
                    transport_failed: false,
                };
            }
            Err(_) => {
                return DiagnosticBody {
                    bytes: marked_or_empty(collected, DIAGNOSTIC_TRANSPORT_MARKER),
                    truncated: false,
                    transport_failed: true,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_accumulator_accepts_exact_limit_and_rejects_plus_one() {
        let mut exact = BoundedAccumulator::new(4);
        exact.push_strict(b"ab").unwrap();
        exact.push_strict(b"cd").unwrap();
        assert_eq!(exact.finish(), Bytes::from_static(b"abcd"));

        let mut oversized = BoundedAccumulator::new(4);
        oversized.push_strict(b"abcd").unwrap();
        assert_eq!(
            oversized.push_strict(b"e"),
            Err(BoundedBodyError::TooLarge { limit: 4 })
        );
    }

    #[test]
    fn diagnostic_marker_stays_inside_budget() {
        let limit = DIAGNOSTIC_TRUNCATION_MARKER.len() + 4;
        let mut collected = BoundedAccumulator::new(limit);
        assert!(collected.push_prefix(&vec![b'x'; limit + 1]).unwrap());
        let marked = collected
            .finish_marked(DIAGNOSTIC_TRUNCATION_MARKER)
            .unwrap();
        assert_eq!(marked.len(), limit);
        assert!(marked.ends_with(DIAGNOSTIC_TRUNCATION_MARKER));
    }
}
