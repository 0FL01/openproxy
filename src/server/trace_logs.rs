//! Best-effort, bounded stream diagnostics, independent of the SQLite journal.
//!
//! Formatting stays in the tracing layer. Producers only buffer one complete
//! record and `try_send`; one OS thread owns all filesystem work. Unix storage
//! is confined to directory descriptors, private regular files, and an advisory
//! lock. Unsupported platforms fail open. A timed-out shutdown detaches rather
//! than waiting on filesystem I/O; already-written JSONL prefixes survive it.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;

/// Include enabled! metadata hints as well as events: per-layer interest needs
/// the same exact target/level predicate for both kinds of callsite.
pub fn accepts_stream_trace(meta: &tracing::Metadata<'_>) -> bool {
    meta.target() == "openproxy::chat::stream" && *meta.level() == tracing::Level::TRACE
}

/// Process-local diagnostic losses; shared by producers and their worker.
#[derive(Default)]
pub struct TraceLogCounters {
    dropped: AtomicU64,
    io_errors: AtomicU64,
}

impl TraceLogCounters {
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn io_errors(&self) -> u64 {
        self.io_errors.load(Ordering::Relaxed)
    }

    fn drop_record(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
struct Limits {
    record: usize,
    queue: usize,
    active: u64,
    archives: usize,
    archived: u64,
    compressed: u64,
    reserve: u64,
    retry: Duration,
    #[cfg(test)]
    available: Option<Option<u64>>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            record: 8 * 1024,
            queue: 256,
            active: 8 * 1024 * 1024,
            archives: 8,
            archived: 96 * 1024 * 1024,
            compressed: 16 * 1024 * 1024,
            reserve: 64 * 1024 * 1024,
            retry: Duration::from_secs(60),
            #[cfg(test)]
            available: None,
        }
    }
}

struct Shared {
    closing: AtomicBool,
    counters: Arc<TraceLogCounters>,
}

// Count queued losses even when a thread fails to spawn or a never-started
// guard is dropped by an early CLI return. Draining this bounded queue is local.
struct RecordReceiver {
    inner: Receiver<Vec<u8>>,
    counters: Arc<TraceLogCounters>,
}

impl std::ops::Deref for RecordReceiver {
    type Target = Receiver<Vec<u8>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Drop for RecordReceiver {
    fn drop(&mut self) {
        for _ in self.inner.try_iter() {
            self.counters.drop_record();
        }
    }
}

/// A cloneable, nonblocking tracing formatter destination.
#[derive(Clone)]
pub struct TraceLogMakeWriter {
    tx: SyncSender<Vec<u8>>,
    shared: Arc<Shared>,
    cap: usize,
}

/// One event's bounded buffer. Only Drop submits the entire formatted record.
pub struct TraceLogWriter {
    destination: TraceLogMakeWriter,
    buffer: Vec<u8>,
    oversized: bool,
}

impl<'a> MakeWriter<'a> for TraceLogMakeWriter {
    type Writer = TraceLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TraceLogWriter {
            destination: self.clone(),
            buffer: Vec::new(),
            oversized: false,
        }
    }
}

impl Write for TraceLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.oversized {
            if bytes.len() > self.destination.cap.saturating_sub(self.buffer.len()) {
                self.oversized = true;
                self.buffer = Vec::new();
            } else {
                // Vec's geometric growth may exceed the cap; reserve exactly.
                self.buffer.reserve_exact(bytes.len());
                self.buffer.extend_from_slice(bytes);
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for TraceLogWriter {
    fn drop(&mut self) {
        if self.buffer.is_empty() && !self.oversized {
            return;
        }
        let shared = &self.destination.shared;
        if self.oversized
            || self.buffer.last() != Some(&b'\n')
            || shared.closing.load(Ordering::Acquire)
        {
            shared.counters.drop_record();
            return;
        }
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = self
            .destination
            .tx
            .try_send(std::mem::take(&mut self.buffer))
        {
            shared.counters.drop_record();
        }
    }
}

/// Worker lifetime and bounded draining; no filesystem work before `start`.
pub struct TraceLogGuard {
    shared: Arc<Shared>,
    rx: Option<RecordReceiver>,
    worker: Option<JoinHandle<()>>,
    done: Option<Receiver<()>>,
    stop: Option<mpsc::Sender<Instant>>,
    limits: Limits,
    started: bool,
    shutdown_attempted: bool,
}

pub fn trace_log_channel() -> (TraceLogMakeWriter, TraceLogGuard) {
    channel_with_limits(Limits::default())
}

fn channel_with_limits(limits: Limits) -> (TraceLogMakeWriter, TraceLogGuard) {
    let (tx, rx) = mpsc::sync_channel(limits.queue);
    let shared = Arc::new(Shared {
        closing: AtomicBool::new(false),
        counters: Arc::new(TraceLogCounters::default()),
    });
    (
        TraceLogMakeWriter {
            tx,
            shared: shared.clone(),
            cap: limits.record,
        },
        TraceLogGuard {
            rx: Some(RecordReceiver {
                inner: rx,
                counters: shared.counters.clone(),
            }),
            shared,
            worker: None,
            done: None,
            stop: None,
            limits,
            started: false,
            shutdown_attempted: false,
        },
    )
}

impl TraceLogGuard {
    /// Spawn exactly one worker, using the directory of the loaded database.
    /// Storage failures are reported by that worker and never refuse startup.
    pub fn start(&mut self, data_dir: &Path) -> io::Result<()> {
        if self.started || self.shared.closing.load(Ordering::Acquire) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        let rx = self
            .rx
            .take()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        let (done_tx, done_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let shared = self.shared.clone();
        let limits = self.limits;
        let data_dir = data_dir.to_path_buf();
        match thread::Builder::new()
            .name("stream-trace".into())
            .spawn(move || {
                run_worker(data_dir, rx, stop_rx, &shared, limits);
                let _ = done_tx.send(());
            }) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.done = Some(done_rx);
                self.stop = Some(stop_tx);
                self.started = true;
                Ok(())
            }
            Err(error) => {
                self.shared.closing.store(true, Ordering::Release);
                self.shared
                    .counters
                    .io_errors
                    .fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    pub fn counters(&self) -> Arc<TraceLogCounters> {
        self.shared.counters.clone()
    }

    /// Close producer admission, drain until the deadline, and only join an
    /// already-completed worker. Blocking filesystem calls cannot be cancelled.
    pub fn shutdown_with_budget(&mut self, budget: Duration) -> bool {
        self.shutdown_attempted = true;
        self.shared.closing.store(true, Ordering::Release);
        let deadline = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(Instant::now);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(deadline);
        }
        if !self.started {
            if let Some(rx) = self.rx.take() {
                for _ in rx.try_iter() {
                    self.shared.counters.drop_record();
                }
            }
            return true;
        }
        let complete = self.done.as_ref().is_none_or(|done| {
            done.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok()
        });
        if complete {
            self.done = None;
            if let Some(worker) = self.worker.take_if(|worker| worker.is_finished()) {
                let _ = worker.join();
            }
        }
        complete
    }
}

impl Drop for TraceLogGuard {
    fn drop(&mut self) {
        if self.started && !self.shutdown_attempted {
            let _ = self.shutdown_with_budget(Duration::from_secs(2));
        } else {
            self.shared.closing.store(true, Ordering::Release);
        }
    }
}

struct Failure {
    stage: &'static str,
    error: io::Error,
}

impl Failure {
    fn at(stage: &'static str, error: io::Error) -> Self {
        Self { stage, error }
    }

    fn kind(stage: &'static str, kind: io::ErrorKind) -> Self {
        Self::at(stage, io::Error::from(kind))
    }
}

type StorageResult<T> = Result<T, Failure>;

struct Reporter {
    counters: Arc<TraceLogCounters>,
    warned: Option<Instant>,
    seen_drops: u64,
}

impl Reporter {
    fn failure(&mut self, failure: &Failure) {
        self.counters.io_errors.fetch_add(1, Ordering::Relaxed);
        self.warn(failure.stage, error_kind(failure.error.kind()));
    }

    fn warn(&mut self, stage: &'static str, kind: &'static str) {
        if self
            .warned
            .is_some_and(|last| last.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        self.warned = Some(Instant::now());
        let dropped = self.counters.dropped();
        let io_errors = self.counters.io_errors();
        tracing::warn!(target: "openproxy::trace_logs", stage, kind, dropped, io_errors,
            "stream trace diagnostic delivery degraded");
        // stderr itself may be closed/full; diagnostics must never panic.
        let _ = writeln!(
            io::stderr().lock(),
            "stream-trace stage={stage} kind={kind} dropped={dropped} io_errors={io_errors}"
        );
    }

    fn losses(&mut self) {
        let dropped = self.counters.dropped();
        if dropped != self.seen_drops {
            self.seen_drops = dropped;
            self.warn("delivery", "dropped");
        }
    }
}

fn error_kind(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "not-found",
        io::ErrorKind::PermissionDenied => "permission-denied",
        io::ErrorKind::AlreadyExists => "already-exists",
        io::ErrorKind::WouldBlock => "would-block",
        io::ErrorKind::InvalidData => "invalid-data",
        io::ErrorKind::InvalidInput => "invalid-input",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::WriteZero => "write-zero",
        io::ErrorKind::UnexpectedEof => "unexpected-eof",
        io::ErrorKind::StorageFull => "storage-full",
        _ => "io-failure",
    }
}

fn enough_space(available: Option<u64>, reserve: u64, allocation: u64) -> bool {
    reserve
        .checked_add(allocation)
        .zip(available)
        .is_some_and(|(required, available)| available >= required)
}

fn write_or_rollback<W: Write>(
    writer: &mut W,
    record: &[u8],
    rollback: impl FnOnce(&mut W) -> io::Result<()>,
) -> Result<(), (io::Error, bool)> {
    writer
        .write_all(record)
        .map_err(|error| (error, rollback(writer).is_err()))
}

fn run_worker(
    data_dir: PathBuf,
    rx: RecordReceiver,
    stop: Receiver<Instant>,
    shared: &Shared,
    limits: Limits,
) {
    let mut reporter = Reporter {
        counters: shared.counters.clone(),
        warned: None,
        seen_drops: 0,
    };
    let mut storage = None;
    let mut attempted = None;
    let mut deadline = None;
    loop {
        if let Ok(at) = stop.try_recv() {
            deadline = Some(at);
        }
        if deadline.is_some_and(|at| Instant::now() >= at) {
            for _ in rx.try_iter() {
                shared.counters.drop_record();
            }
            break;
        }
        if storage.is_none() && attempted.is_none_or(|last: Instant| last.elapsed() >= limits.retry)
        {
            attempted = Some(Instant::now());
            match Storage::open(&data_dir, limits) {
                Ok(opened) => storage = Some(opened),
                Err(error) => reporter.failure(&error),
            }
        }
        if let Some(Err(error)) = storage.as_mut().map(Storage::maintain) {
            reporter.failure(&error);
        }
        reporter.losses();
        // No busy idle polling; producer sends still wake the worker immediately.
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(record) => {
                let result = match storage.as_mut() {
                    Some(store) => store.append(&record, &mut reporter),
                    None => Err(Failure::kind("unavailable", io::ErrorKind::WouldBlock)),
                };
                if let Err(error) = result {
                    shared.counters.drop_record();
                    if error.stage != "unavailable" && error.stage != "pending" {
                        reporter.failure(&error);
                    }
                    // Back off failed writes/probes even when rollback succeeded.
                    // Otherwise a full disk is retried for every incoming event.
                    if matches!(error.stage, "space" | "write")
                        || storage.as_ref().is_some_and(Storage::needs_reopen)
                    {
                        storage = None;
                        attempted = Some(Instant::now());
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if shared.closing.load(Ordering::Acquire) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Streaming compressed output cap, including encoder finish/footer bytes.
struct CappedWriter<W> {
    inner: W,
    written: u64,
    cap: u64,
}

impl<W: Write> Write for CappedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.cap.saturating_sub(self.written) {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        let count = self.inner.write(bytes)?;
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn compress<R: io::Read, W: Write>(
    source: R,
    destination: W,
    raw_cap: u64,
    output_cap: u64,
) -> io::Result<CappedWriter<W>> {
    let output = CappedWriter {
        inner: destination,
        written: 0,
        cap: output_cap,
    };
    let mut encoder = zstd::stream::write::Encoder::new(output, 3)?;
    encoder.include_checksum(true)?;
    let copied = io::copy(&mut source.take(raw_cap.saturating_add(1)), &mut encoder)?;
    if copied > raw_cap {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    encoder.finish()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix_storage {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::{CStr, CString};
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    struct Directory(File);

    fn c_name(name: &str) -> io::Result<CString> {
        CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    }

    impl Directory {
        fn child(parent: &File, name: &str) -> io::Result<File> {
            let name = c_name(name)?;
            // SAFETY: parent is an open directory and name is a valid C string.
            if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            // SAFETY: valid descriptor/string; returned descriptor is owned.
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful openat returned a new owned descriptor.
            let file = unsafe { File::from_raw_fd(fd) };
            if file.metadata()?.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            Ok(file)
        }

        fn open(data_dir: &Path) -> io::Result<Self> {
            let root = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(data_dir)?;
            let logs = Self::child(&root, "logs")?;
            let leaf = Self::child(&logs, "stream-trace")?;
            // SAFETY: leaf is our verified, owned directory descriptor.
            if unsafe { libc::fchmod(leaf.as_raw_fd(), 0o700) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(leaf))
        }

        fn open_file(&self, name: &str, flags: i32) -> io::Result<File> {
            let name = c_name(name)?;
            // O_NONBLOCK also prevents a malicious FIFO from blocking open.
            // SAFETY: valid descriptor/string; creation mode is always private.
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    name.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful openat returned a new owned descriptor.
            let file = unsafe { File::from_raw_fd(fd) };
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            // SAFETY: a verified private regular file; never a linked database.
            if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(file)
        }

        fn remove(&self, name: &str) -> io::Result<()> {
            // Verify before unlink; unrelated/nonregular/linked files stay put.
            let _file = self.open_file(name, libc::O_RDONLY)?;
            let name = c_name(name)?;
            // SAFETY: valid directory and checked, canonical filename.
            if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        fn rename_new(&self, from: &str, to: &str) -> io::Result<()> {
            let from = c_name(from)?;
            let to = c_name(to)?;
            let fd = self.0.as_raw_fd();
            #[cfg(target_os = "linux")]
            // SAFETY: valid directory and C strings; NOREPLACE is atomic.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    fd,
                    from.as_ptr(),
                    fd,
                    to.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            #[cfg(target_os = "macos")]
            // SAFETY: valid directory and C strings; RENAME_EXCL is atomic.
            let result = unsafe {
                libc::renameatx_np(fd, from.as_ptr(), fd, to.as_ptr(), libc::RENAME_EXCL)
            };
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            return Err(io::Error::from(io::ErrorKind::Unsupported));
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Ok(())
        }

        fn entries(&self) -> io::Result<Entries> {
            // Use a fresh open description, not dup: directory offsets must not
            // be shared with previous scans. Never collect a directory in RAM.
            let dot = c_name(".")?;
            // SAFETY: valid directory descriptor and constant child path.
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    dot.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is a newly owned directory descriptor.
            let dir = unsafe { libc::fdopendir(fd) };
            if dir.is_null() {
                // SAFETY: fdopendir did not take ownership on failure.
                unsafe { libc::close(fd) };
                return Err(io::Error::last_os_error());
            }
            Ok(Entries {
                dir,
                exhausted: false,
            })
        }
    }

    struct Entries {
        dir: *mut libc::DIR,
        exhausted: bool,
    }

    impl Iterator for Entries {
        type Item = io::Result<String>;

        fn next(&mut self) -> Option<Self::Item> {
            if self.exhausted {
                return None;
            }
            loop {
                #[cfg(target_os = "linux")]
                // SAFETY: libc supplies this thread's live errno location.
                let errno = unsafe { libc::__errno_location() };
                #[cfg(target_os = "macos")]
                // SAFETY: libc supplies this thread's live errno location.
                let errno = unsafe { libc::__error() };
                // SAFETY: errno is thread-local; clear it to distinguish EOF
                // from a failed directory scan. Partial scans must fail open.
                unsafe { *errno = 0 };
                // SAFETY: live DIR, exclusively used by this worker.
                let entry = unsafe { libc::readdir(self.dir) };
                if entry.is_null() {
                    self.exhausted = true;
                    // SAFETY: errno remains the current thread's live slot.
                    let code = unsafe { *errno };
                    return if code == 0 {
                        None
                    } else {
                        Some(Err(io::Error::from_raw_os_error(code)))
                    };
                }
                // SAFETY: readdir supplies a NUL-terminated filename, copied
                // before another readdir call can invalidate the entry.
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                if let Ok(name) = name.to_str() {
                    return Some(Ok(name.to_owned()));
                }
            }
        }
    }

    impl Drop for Entries {
        fn drop(&mut self) {
            // SAFETY: this iterator uniquely owns the live DIR and its fd.
            unsafe { libc::closedir(self.dir) };
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum SegmentKind {
        Archive,
        Pending,
        Temporary,
    }

    fn segment(name: &str) -> Option<(u64, SegmentKind)> {
        let (number, kind) = if let Some(number) = name.strip_suffix(".jsonl.zst.tmp") {
            (number, SegmentKind::Temporary)
        } else if let Some(number) = name.strip_suffix(".pending.jsonl") {
            (number, SegmentKind::Pending)
        } else {
            (name.strip_suffix(".jsonl.zst")?, SegmentKind::Archive)
        };
        if number.len() != 20 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some((number.parse().ok()?, kind))
    }

    fn archive_name(id: u64) -> String {
        format!("{id:020}.jsonl.zst")
    }

    fn pending_name(id: u64) -> String {
        format!("{id:020}.pending.jsonl")
    }

    fn temporary_name(id: u64) -> String {
        format!("{id:020}.jsonl.zst.tmp")
    }

    struct Active {
        file: File,
        committed: u64,
    }

    pub(super) struct Storage {
        directory: Directory,
        _lock: File,
        data_dir: PathBuf,
        limits: Limits,
        active: Option<Active>,
        archives: BTreeMap<u64, u64>,
        pending: Option<u64>,
        next: Option<u64>,
        compression_attempt: Option<Instant>,
    }

    impl Storage {
        pub(super) fn open(data_dir: &Path, limits: Limits) -> StorageResult<Self> {
            let directory =
                Directory::open(data_dir).map_err(|error| Failure::at("directory", error))?;
            let lock = directory
                .open_file(".lock", libc::O_RDWR | libc::O_CREAT)
                .map_err(|error| Failure::at("lock", error))?;
            if lock
                .metadata()
                .map_err(|error| Failure::at("lock", error))?
                .len()
                != 0
            {
                return Err(Failure::kind("lock", io::ErrorKind::InvalidData));
            }
            // SAFETY: an open, verified zero-byte regular lock file. Its fd is
            // retained until this Storage drops, so exclusion covers recovery.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(Failure::at("lock", io::Error::last_os_error()));
            }
            let mut store = Self {
                directory,
                _lock: lock,
                data_dir: data_dir.to_path_buf(),
                limits,
                active: None,
                archives: BTreeMap::new(),
                pending: None,
                next: Some(0),
                compression_attempt: None,
            };
            store
                .scan()
                .map_err(|error| Failure::at("recovery", error))?;
            let mut file = store
                .directory
                .open_file("active.jsonl", libc::O_RDWR | libc::O_CREAT)
                .map_err(|error| Failure::at("active", error))?;
            let committed =
                repair_tail(&mut file, limits).map_err(|error| Failure::at("tail", error))?;
            store.active = Some(Active { file, committed });
            Ok(store)
        }

        fn scan(&mut self) -> io::Result<()> {
            let mut largest = None;
            for name in self.directory.entries()? {
                let name = name?;
                let Some((id, kind)) = segment(&name) else {
                    continue;
                };
                largest = Some(largest.map_or(id, |old: u64| old.max(id)));
                let file = self.directory.open_file(&name, libc::O_RDONLY)?;
                let len = file.metadata()?.len();
                drop(file);
                match kind {
                    SegmentKind::Temporary => self.directory.remove(&name)?,
                    SegmentKind::Archive => {
                        if len > self.limits.compressed || len > self.limits.archived {
                            self.directory.remove(&name)?;
                            continue;
                        }
                        if self.limits.archives == 0 {
                            self.directory.remove(&name)?;
                            continue;
                        }
                        // Retain at most eight metadata entries even while
                        // scanning an arbitrarily large directory, newest first.
                        while self.archives.len() >= self.limits.archives
                            || self.archived_bytes() > self.limits.archived - len
                        {
                            if self
                                .archives
                                .first_key_value()
                                .is_some_and(|(&old, _)| id < old)
                            {
                                break;
                            }
                            self.prune_one()?;
                        }
                        if self.archives.len() >= self.limits.archives
                            || self.archived_bytes() > self.limits.archived - len
                        {
                            self.directory.remove(&name)?;
                        } else {
                            self.archives.insert(id, len);
                        }
                    }
                    SegmentKind::Pending => {
                        if len > self.limits.active {
                            return Err(io::Error::from(io::ErrorKind::InvalidData));
                        }
                        // A compliant worker only ever creates one pending raw.
                        // Refuse unexpected extra raws rather than deleting an
                        // unpublished diagnostic prefix to make room.
                        if self.pending.is_some() {
                            return Err(io::Error::from(io::ErrorKind::InvalidData));
                        }
                        self.pending = Some(id);
                    }
                }
            }
            self.next = largest.map_or(Some(0), |id| id.checked_add(1));
            Ok(())
        }

        fn archived_bytes(&self) -> u64 {
            self.archives.values().copied().sum()
        }

        fn prune(&mut self, count: usize, bytes: u64) -> io::Result<()> {
            while self.archives.len() > count || self.archived_bytes() > bytes {
                self.prune_one()?;
            }
            Ok(())
        }

        fn prune_one(&mut self) -> io::Result<()> {
            if let Some((&id, _)) = self.archives.first_key_value() {
                self.directory.remove(&archive_name(id))?;
                self.archives.remove(&id);
            }
            Ok(())
        }

        fn available(&self) -> Option<u64> {
            #[cfg(test)]
            if let Some(available) = self.limits.available {
                return available;
            }
            crate::server::application_logs::data_dir_avail_bytes(&self.data_dir)
        }

        fn ensure_space(&mut self, bytes: u64) -> io::Result<()> {
            loop {
                let available = self
                    .available()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
                // Unknown capacity is not evidence that deleting history helps.
                if enough_space(Some(available), self.limits.reserve, bytes) {
                    return Ok(());
                }
                if self.archives.is_empty() {
                    return Err(io::Error::from(io::ErrorKind::StorageFull));
                }
                self.prune_one()?;
            }
        }

        pub(super) fn maintain(&mut self) -> StorageResult<()> {
            if self.pending.is_none()
                || self
                    .compression_attempt
                    .is_some_and(|last| last.elapsed() < self.limits.retry)
            {
                return Ok(());
            }
            self.compression_attempt = Some(Instant::now());
            self.publish_pending()
                .map_err(|error| Failure::at("compression", error))?;
            self.compression_attempt = None;
            Ok(())
        }

        fn publish_pending(&mut self) -> io::Result<()> {
            let Some(id) = self.pending else {
                return Ok(());
            };
            let raw_name = pending_name(id);
            let final_name = archive_name(id);
            let tmp_name = temporary_name(id);
            let raw = self.directory.open_file(&raw_name, libc::O_RDONLY)?;
            let raw_len = raw.metadata()?.len();
            if raw_len > self.limits.active {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            match self.directory.open_file(&final_name, libc::O_RDONLY) {
                Ok(final_file) => {
                    if validate_archive(final_file, raw_len, self.limits).is_ok() {
                        self.directory.remove(&raw_name)?;
                        self.pending = None;
                        return Ok(());
                    }
                    self.directory.remove(&final_name)?;
                    self.archives.remove(&id);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            // Reserve the entire bounded output slot before allocating it.
            if self.limits.archives == 0 || self.limits.archived < self.limits.compressed {
                return Err(io::Error::from(io::ErrorKind::StorageFull));
            }
            self.prune(
                self.limits.archives - 1,
                self.limits.archived - self.limits.compressed,
            )?;
            self.ensure_space(self.limits.compressed)?;
            match self.directory.remove(&tmp_name) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let output = self
                .directory
                .open_file(&tmp_name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
            let result = (|| {
                let compressed = compress(raw, output, self.limits.active, self.limits.compressed)?;
                compressed.inner.sync_all()?;
                let len = compressed.written;
                drop(compressed);
                self.directory.rename_new(&tmp_name, &final_name)?;
                self.archives.insert(id, len);
                // A crash here is recovered by bounded decoding with checksum.
                self.directory.remove(&raw_name)?;
                self.pending = None;
                Ok(())
            })();
            if result.is_err() {
                // If unlink fails, keep this one bounded tmp; next retry must
                // remove it successfully before creating another output.
                let _ = self.directory.remove(&tmp_name);
            }
            result
        }

        pub(super) fn append(
            &mut self,
            record: &[u8],
            reporter: &mut Reporter,
        ) -> StorageResult<()> {
            if record.len() > self.limits.record || record.len() as u64 > self.limits.active {
                return Err(Failure::kind("record", io::ErrorKind::InvalidData));
            }
            let active_len = self
                .active
                .as_ref()
                .ok_or_else(|| Failure::kind("active", io::ErrorKind::InvalidData))?
                .committed;
            if active_len.saturating_add(record.len() as u64) > self.limits.active {
                if self.pending.is_some() {
                    return Err(Failure::kind("pending", io::ErrorKind::WouldBlock));
                }
                self.rotate()
                    .map_err(|error| Failure::at("rotation", error))?;
                if let Err(error) = self.maintain() {
                    reporter.failure(&error);
                }
            }
            self.ensure_space(record.len() as u64)
                .map_err(|error| Failure::at("space", error))?;
            let active = self
                .active
                .as_mut()
                .ok_or_else(|| Failure::kind("active", io::ErrorKind::InvalidData))?;
            let committed = active.committed;
            if let Err((error, rollback_failed)) =
                write_or_rollback(&mut active.file, record, |file| rollback(file, committed))
            {
                // Restore the last complete JSONL prefix after partial writes.
                if rollback_failed {
                    self.active = None;
                }
                return Err(Failure::at("write", error));
            }
            active.committed += record.len() as u64;
            Ok(())
        }

        fn rotate(&mut self) -> io::Result<()> {
            let id = self
                .next
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            self.next = id.checked_add(1);
            // Close before rename, without a per-event fsync/BufWriter.
            self.active = None;
            self.directory
                .rename_new("active.jsonl", &pending_name(id))?;
            self.pending = Some(id);
            let file = self
                .directory
                .open_file("active.jsonl", libc::O_RDWR | libc::O_CREAT | libc::O_EXCL)?;
            self.active = Some(Active { file, committed: 0 });
            Ok(())
        }

        pub(super) fn needs_reopen(&self) -> bool {
            self.active.is_none()
        }
    }

    fn rollback(file: &mut File, committed: u64) -> io::Result<()> {
        file.set_len(committed)?;
        file.seek(SeekFrom::Start(committed))?;
        Ok(())
    }

    fn repair_tail(file: &mut File, limits: Limits) -> io::Result<u64> {
        let len = file.metadata()?.len();
        if len > limits.active {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let start = len.saturating_sub(limits.record as u64);
        file.seek(SeekFrom::Start(start))?;
        let mut tail = vec![0; (len - start) as usize];
        file.read_exact(&mut tail)?;
        let committed = match tail.iter().rposition(|byte| *byte == b'\n') {
            Some(end) => start + end as u64 + 1,
            None if start == 0 => 0,
            None => return Err(io::Error::from(io::ErrorKind::InvalidData)),
        };
        rollback(file, committed)?;
        Ok(committed)
    }

    fn validate_archive(mut file: File, raw_len: u64, limits: Limits) -> io::Result<()> {
        if file.metadata()?.len() > limits.compressed || raw_len > limits.active {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        // Our archive format is a checksummed zstd frame. Refuse a substituted
        // checksum-free frame before treating a matching raw as published.
        let mut header = [0; 5];
        file.read_exact(&mut header)?;
        if header[..4] != [0x28, 0xb5, 0x2f, 0xfd] || header[4] & 4 == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        file.seek(SeekFrom::Start(0))?;
        let mut decoder =
            zstd::stream::read::Decoder::new(file.take(limits.compressed.saturating_add(1)))?;
        // A damaged frame header cannot request an unbounded decoder window.
        decoder.window_log_max(23)?;
        let decoded = io::copy(
            &mut decoder.take(limits.active.saturating_add(1)),
            &mut io::sink(),
        )?;
        if decoded != raw_len {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs;
        use std::os::unix::fs::{symlink, PermissionsExt};
        use tempfile::TempDir;

        fn limits() -> Limits {
            Limits {
                record: 128,
                queue: 4,
                active: 256,
                archives: 3,
                archived: 1024,
                compressed: 384,
                reserve: 0,
                retry: Duration::from_millis(1),
                available: Some(Some(u64::MAX)),
            }
        }

        fn open(temp: &TempDir, limits: Limits) -> Storage {
            Storage::open(temp.path(), limits).unwrap_or_else(|failure| {
                panic!("{}: {}", failure.stage, error_kind(failure.error.kind()))
            })
        }

        fn reporter() -> Reporter {
            Reporter {
                counters: Arc::new(TraceLogCounters::default()),
                warned: None,
                seen_drops: 0,
            }
        }

        fn leaf(temp: &TempDir) -> PathBuf {
            temp.path().join("logs/stream-trace")
        }

        fn record(number: usize) -> Vec<u8> {
            format!("{{\"n\":{number},\"text\":\"{}\"}}\n", "界🌍".repeat(5)).into_bytes()
        }

        fn append(store: &mut Storage, record: &[u8]) {
            let result = store.append(record, &mut reporter());
            assert!(result.is_ok(), "append failed");
        }

        fn encoded(raw: &[u8]) -> Vec<u8> {
            compress(raw, Vec::new(), raw.len() as u64, 4096)
                .unwrap()
                .inner
        }

        fn decoded(path: &Path) -> Vec<u8> {
            let mut decoder = zstd::stream::read::Decoder::new(File::open(path).unwrap()).unwrap();
            let mut bytes = Vec::new();
            decoder.read_to_end(&mut bytes).unwrap();
            bytes
        }

        fn check_caps(temp: &TempDir, limits: Limits) {
            let mut archives = 0;
            let mut archive_bytes = 0;
            let mut pending = 0;
            let mut temporary = 0;
            let mut total = 0;
            for entry in fs::read_dir(leaf(temp)).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_str().unwrap().to_owned();
                let len = entry.metadata().unwrap().len();
                if name == "active.jsonl" {
                    assert!(len <= limits.active);
                    total += len;
                } else if let Some((_, kind)) = segment(&name) {
                    total += len;
                    match kind {
                        SegmentKind::Archive => {
                            archives += 1;
                            archive_bytes += len;
                            assert!(len <= limits.compressed);
                        }
                        SegmentKind::Pending => {
                            pending += 1;
                            assert!(len <= limits.active);
                        }
                        SegmentKind::Temporary => {
                            temporary += 1;
                            assert!(len <= limits.compressed);
                        }
                    }
                }
            }
            assert!(archives <= limits.archives);
            assert!(archive_bytes <= limits.archived);
            assert!(pending <= 1);
            assert!(temporary <= 1);
            assert!(total <= limits.active * 2 + limits.archived + limits.compressed);
        }

        #[test]
        fn rotation_crc_unicode_order_and_all_payload_caps() {
            let temp = TempDir::new().unwrap();
            let limits = limits();
            let mut store = open(&temp, limits);
            for number in 0..100 {
                append(&mut store, &record(number));
                check_caps(&temp, limits);
            }
            let mut all = Vec::new();
            for &id in store.archives.keys() {
                all.extend(decoded(&leaf(&temp).join(archive_name(id))));
            }
            all.extend(fs::read(leaf(&temp).join("active.jsonl")).unwrap());
            let mut previous = None;
            for line in String::from_utf8(all).unwrap().lines() {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                let number = value["n"].as_u64().unwrap();
                assert!(previous.is_none_or(|previous| number == previous + 1));
                assert_eq!(value["text"], "界🌍".repeat(5));
                previous = Some(number);
            }
            assert_eq!(previous, Some(99));
            assert_eq!(
                fs::metadata(leaf(&temp)).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for entry in fs::read_dir(leaf(&temp)).unwrap() {
                assert_eq!(
                    entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }

        #[test]
        fn incompressible_records_output_cap_keeps_single_pending_and_stops_at_active_cap() {
            let temp = TempDir::new().unwrap();
            let mut limits = limits();
            limits.compressed = 24;
            limits.active = 128;
            let mut store = open(&temp, limits);
            // A deterministic pseudorandom ASCII string: no valid JSON is split.
            let mut state = 7u64;
            let text: String = (0..90)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (b'a' + (state % 26) as u8) as char
                })
                .collect();
            let raw = format!("{{\"text\":\"{text}\"}}\n").into_bytes();
            append(&mut store, &raw);
            append(&mut store, &raw);
            let pending = leaf(&temp).join(pending_name(0));
            assert_eq!(fs::read(&pending).unwrap(), raw);
            let failure = store.append(&raw, &mut reporter()).err().unwrap();
            assert_eq!(failure.stage, "pending");
            assert_eq!(fs::read(&pending).unwrap(), raw);
            assert!(!leaf(&temp).join(temporary_name(0)).exists());
            check_caps(&temp, limits);
            // Failed compression retries are local and throttled, not triggered
            // again by every subsequent rejected diagnostic.
            let attempted = store.compression_attempt;
            store.limits.retry = Duration::from_secs(60);
            assert!(store.maintain().is_ok());
            assert_eq!(store.compression_attempt, attempted);
        }

        #[test]
        fn byte_budget_prunes_before_new_compressed_slot() {
            let temp = TempDir::new().unwrap();
            let mut limits = limits();
            limits.active = 128;
            limits.archives = 8;
            limits.archived = 250;
            limits.compressed = 180;
            let mut store = open(&temp, limits);
            for number in 0..50 {
                append(&mut store, &record(number));
                check_caps(&temp, limits);
            }
            assert!(store.archived_bytes() <= 250);
            assert!(
                store.archives.len() < 8,
                "bytes, not just count, must cause pruning"
            );
        }

        #[test]
        fn partial_tail_recovery_preserves_complete_utf8_json_prefix() {
            let temp = TempDir::new().unwrap();
            drop(open(&temp, limits()));
            let first = record(0);
            let next = record(1);
            let mut raw = first.clone();
            raw.extend_from_slice(&next[..next.len() - 5]);
            fs::write(leaf(&temp).join("active.jsonl"), raw).unwrap();
            let mut store = open(&temp, limits());
            assert_eq!(store.active.as_ref().unwrap().committed, first.len() as u64);
            append(&mut store, &next);
            assert_eq!(
                fs::read(leaf(&temp).join("active.jsonl")).unwrap(),
                [first, next].concat()
            );
            drop(store);
            fs::write(leaf(&temp).join("active.jsonl"), b"{\"unfinished\":").unwrap();
            let store = open(&temp, limits());
            assert_eq!(store.active.as_ref().unwrap().committed, 0);
        }

        #[test]
        fn startup_recovers_pending_removes_tmp_and_uses_largest_id() {
            let temp = TempDir::new().unwrap();
            drop(open(&temp, limits()));
            let raw = record(7);
            fs::write(leaf(&temp).join(pending_name(4)), &raw).unwrap();
            fs::write(leaf(&temp).join(temporary_name(9)), b"unfinished").unwrap();
            fs::write(leaf(&temp).join("notes.sqlite"), b"unrelated").unwrap();
            check_caps(&temp, limits()); // includes both pending and tmp payloads.
            let mut store = open(&temp, limits());
            assert_eq!(store.next, Some(10));
            assert!(!leaf(&temp).join(temporary_name(9)).exists());
            assert!(store.maintain().is_ok());
            assert_eq!(decoded(&leaf(&temp).join(archive_name(4))), raw);
            assert!(!leaf(&temp).join(pending_name(4)).exists());
            assert_eq!(
                fs::read(leaf(&temp).join("notes.sqlite")).unwrap(),
                b"unrelated"
            );
        }

        #[test]
        fn raw_plus_published_final_recovery_validates_checksum_and_length() {
            for corruption in 0..4 {
                let temp = TempDir::new().unwrap();
                drop(open(&temp, limits()));
                let raw = record(3);
                let mut final_bytes = encoded(&raw);
                match corruption {
                    1 => {
                        let last = final_bytes.len() - 1;
                        final_bytes[last] ^= 0x40; // CRC, not a decompressed byte.
                    }
                    2 => final_bytes = encoded(b"{}\n"), // valid checksum, wrong raw length.
                    3 => final_bytes = zstd::stream::encode_all(raw.as_slice(), 3).unwrap(),
                    _ => {}
                }
                fs::write(leaf(&temp).join(pending_name(2)), &raw).unwrap();
                fs::write(leaf(&temp).join(archive_name(2)), final_bytes.clone()).unwrap();
                let mut store = open(&temp, limits());
                assert!(store.maintain().is_ok());
                assert!(!leaf(&temp).join(pending_name(2)).exists());
                assert_eq!(decoded(&leaf(&temp).join(archive_name(2))), raw);
                if corruption == 0 {
                    assert_eq!(
                        fs::read(leaf(&temp).join(archive_name(2))).unwrap(),
                        final_bytes
                    );
                }
                check_caps(&temp, limits());
            }
        }

        #[test]
        fn corrupt_final_and_failed_finish_never_discard_raw() {
            let temp = TempDir::new().unwrap();
            drop(open(&temp, limits()));
            let raw = record(3);
            fs::write(leaf(&temp).join(pending_name(2)), &raw).unwrap();
            fs::write(leaf(&temp).join(archive_name(2)), b"bad-zstd").unwrap();
            let mut tiny = limits();
            tiny.compressed = 8;
            let mut store = open(&temp, tiny);
            assert!(store.maintain().is_err());
            assert_eq!(fs::read(leaf(&temp).join(pending_name(2))).unwrap(), raw);
            assert!(!leaf(&temp).join(archive_name(2)).exists());
            assert!(!leaf(&temp).join(temporary_name(2)).exists());
            check_caps(&temp, tiny);
        }

        #[test]
        fn startup_scan_is_bounded_and_preserves_noncanonical_names() {
            let temp = TempDir::new().unwrap();
            drop(open(&temp, limits()));
            for id in (0..100).rev() {
                fs::write(leaf(&temp).join(archive_name(id)), encoded(b"{}\n")).unwrap();
            }
            for name in [
                "123.jsonl.zst",
                "123.pending.jsonl",
                "notes",
                "18446744073709551616.jsonl.zst",
            ] {
                fs::write(leaf(&temp).join(name), b"untouched").unwrap();
            }
            let store = open(&temp, limits());
            assert_eq!(
                store.archives.keys().copied().collect::<Vec<_>>(),
                vec![97, 98, 99]
            );
            assert_eq!(store.next, Some(100));
            check_caps(&temp, limits());
            for name in [
                "123.jsonl.zst",
                "123.pending.jsonl",
                "notes",
                "18446744073709551616.jsonl.zst",
            ] {
                assert_eq!(fs::read(leaf(&temp).join(name)).unwrap(), b"untouched");
            }
        }

        #[test]
        fn symlink_components_files_and_hardlinks_are_rejected_without_touching_target() {
            for component in ["logs", "stream-trace"] {
                let temp = TempDir::new().unwrap();
                let outside = TempDir::new().unwrap();
                let link = if component == "logs" {
                    temp.path().join("logs")
                } else {
                    fs::create_dir(temp.path().join("logs")).unwrap();
                    temp.path().join("logs/stream-trace")
                };
                symlink(outside.path(), link).unwrap();
                assert!(Storage::open(temp.path(), limits()).is_err());
                assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
            }
            for name in [
                "active.jsonl".to_owned(),
                ".lock".to_owned(),
                archive_name(1),
                pending_name(1),
                temporary_name(1),
            ] {
                for hardlink in [false, true] {
                    let temp = TempDir::new().unwrap();
                    drop(open(&temp, limits()));
                    let victim = temp.path().join("database-fixture");
                    fs::write(&victim, b"preserve database bytes").unwrap();
                    let managed = leaf(&temp).join(&name);
                    if managed.exists() {
                        fs::remove_file(&managed).unwrap();
                    }
                    if hardlink {
                        fs::hard_link(&victim, &managed).unwrap();
                    } else {
                        symlink(&victim, &managed).unwrap();
                    }
                    assert!(Storage::open(temp.path(), limits()).is_err());
                    assert_eq!(fs::read(&victim).unwrap(), b"preserve database bytes");
                    assert!(managed.symlink_metadata().is_ok());
                }
            }
        }

        #[test]
        fn second_lock_is_nonblocking_and_never_truncates_first_writer() {
            let temp = TempDir::new().unwrap();
            let mut store = open(&temp, limits());
            append(&mut store, &record(0));
            let before = Instant::now();
            let failure = Storage::open(temp.path(), limits()).err().unwrap();
            assert_eq!(failure.stage, "lock");
            assert!(before.elapsed() < Duration::from_millis(250));
            assert_eq!(
                fs::read(leaf(&temp).join("active.jsonl")).unwrap(),
                record(0)
            );
            drop(store);
            assert!(Storage::open(temp.path(), limits()).is_ok());
        }

        #[test]
        fn low_space_prunes_but_unknown_capacity_preserves_archives_and_raw() {
            for available in [None, Some(0)] {
                let temp = TempDir::new().unwrap();
                let mut store = open(&temp, limits());
                for number in 0..10 {
                    append(&mut store, &record(number));
                }
                assert!(!store.archives.is_empty());
                let archive_count = store.archives.len();
                let before = fs::read(leaf(&temp).join("active.jsonl")).unwrap();
                store.limits.available = Some(available);
                let failure = store.append(b"{}\n", &mut reporter()).err().unwrap();
                assert_eq!(failure.stage, "space");
                if available.is_some() {
                    assert!(store.archives.is_empty());
                } else {
                    assert_eq!(store.archives.len(), archive_count);
                }
                assert_eq!(fs::read(leaf(&temp).join("active.jsonl")).unwrap(), before);
                // Compression reserves the maximum output, not a guessed ratio.
                store.limits.available = Some(Some(100));
                store.pending = Some(77);
                fs::write(leaf(&temp).join(pending_name(77)), b"{}\n").unwrap();
                assert!(store.maintain().is_err());
                assert!(leaf(&temp).join(pending_name(77)).exists());
                assert!(!leaf(&temp).join(temporary_name(77)).exists());
            }
        }

        #[test]
        fn write_error_and_failed_rollback_degrade_until_recovery_without_panic() {
            let temp = TempDir::new().unwrap();
            let mut store = open(&temp, limits());
            append(&mut store, b"{\"prefix\":1}\n");
            let path = leaf(&temp).join("active.jsonl");
            let active = store.active.as_mut().unwrap();
            active.file = File::open(&path).unwrap(); // EBADF for writes/truncation.
            let failure = store
                .append(b"{\"next\":2}\n", &mut reporter())
                .err()
                .unwrap();
            assert_eq!(failure.stage, "write");
            assert!(store.needs_reopen());
            assert_eq!(fs::read(&path).unwrap(), b"{\"prefix\":1}\n");
            assert!(store.append(b"{}\n", &mut reporter()).is_err());
            drop(store);
            let mut store = open(&temp, limits());
            append(&mut store, b"{\"next\":2}\n");
            assert_eq!(fs::read(&path).unwrap(), b"{\"prefix\":1}\n{\"next\":2}\n");
        }

        #[test]
        fn atomic_publish_never_overwrites_and_sequence_overflow_is_checked() {
            let temp = TempDir::new().unwrap();
            let mut limits = limits();
            limits.active = 128;
            let mut store = open(&temp, limits);
            fs::write(leaf(&temp).join("source"), b"new").unwrap();
            fs::write(leaf(&temp).join("destination"), b"old").unwrap();
            assert!(store.directory.rename_new("source", "destination").is_err());
            assert_eq!(fs::read(leaf(&temp).join("destination")).unwrap(), b"old");
            drop(store);
            fs::write(leaf(&temp).join(temporary_name(u64::MAX)), b"unfinished").unwrap();
            store = open(&temp, limits);
            assert_eq!(store.next, None);
            append(&mut store, &record(1));
            append(&mut store, &record(2));
            assert!(store.append(&record(3), &mut reporter()).is_err());
            assert_eq!(
                fs::read(leaf(&temp).join("active.jsonl")).unwrap(),
                [record(1), record(2)].concat()
            );
        }

        #[test]
        fn excess_unpublished_raws_fail_open_without_deleting_either() {
            let temp = TempDir::new().unwrap();
            drop(open(&temp, limits()));
            for id in [1, 2] {
                fs::write(leaf(&temp).join(pending_name(id)), record(id as usize)).unwrap();
            }
            assert!(Storage::open(temp.path(), limits()).is_err());
            for id in [1, 2] {
                assert_eq!(
                    fs::read(leaf(&temp).join(pending_name(id))).unwrap(),
                    record(id as usize)
                );
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
use unix_storage::Storage;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct Storage;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Storage {
    fn open(_: &Path, _: Limits) -> StorageResult<Self> {
        Err(Failure::kind("platform", io::ErrorKind::Unsupported))
    }
    fn maintain(&mut self) -> StorageResult<()> {
        Ok(())
    }
    fn append(&mut self, _: &[u8], _: &mut Reporter) -> StorageResult<()> {
        Err(Failure::kind("platform", io::ErrorKind::Unsupported))
    }
    fn needs_reopen(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::callsite::Callsite;
    use tracing_subscriber::prelude::*;

    fn send(writer: &TraceLogMakeWriter, bytes: &[u8]) {
        let mut event = writer.make_writer();
        assert_eq!(event.write(bytes).unwrap(), bytes.len());
    }

    #[test]
    fn exact_filter_accepts_event_and_enabled_hint_only_for_trace_target() {
        macro_rules! check {
            ($kind:expr, $target:expr, $level:expr, $accepted:expr) => {{
                let callsite = tracing::callsite! {
                    name: "trace-log-filter", kind: $kind, target: $target, level: $level, fields:
                };
                assert_eq!(accepts_stream_trace(callsite.metadata()), $accepted);
            }};
        }
        check!(
            tracing::metadata::Kind::EVENT,
            "openproxy::chat::stream",
            tracing::Level::TRACE,
            true
        );
        check!(
            tracing::metadata::Kind::HINT,
            "openproxy::chat::stream",
            tracing::Level::TRACE,
            true
        );
        check!(
            tracing::metadata::Kind::EVENT,
            "openproxy::chat::stream",
            tracing::Level::DEBUG,
            false
        );
        check!(
            tracing::metadata::Kind::HINT,
            "openproxy::chat::stream::extra",
            tracing::Level::TRACE,
            false
        );
        check!(
            tracing::metadata::Kind::EVENT,
            "openproxy::other",
            tracing::Level::TRACE,
            false
        );
    }

    #[test]
    fn json_fmt_enabled_hint_unicode_and_whole_oversized_event() {
        let (writer, guard) = trace_log_channel();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(writer)
                .with_filter(tracing_subscriber::filter::filter_fn(accepts_stream_trace)),
        );
        tracing::subscriber::with_default(subscriber, || {
            assert!(tracing::enabled!(target: "openproxy::chat::stream", tracing::Level::TRACE));
            assert!(!tracing::enabled!(target: "openproxy::chat::stream", tracing::Level::DEBUG));
            tracing::trace!(target: "openproxy::chat::stream", text = "Привет 世界 🌍");
            tracing::trace!(target: "openproxy::chat::stream", text = "界".repeat(4000));
            tracing::trace!(target: "openproxy::chat::stream", text = "after oversized");
        });
        let rx = guard.rx.as_ref().unwrap();
        let first = rx.try_recv().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(value["fields"]["text"], "Привет 世界 🌍");
        let value: serde_json::Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(value["fields"]["text"], "after oversized");
        assert!(rx.try_recv().is_err());
        assert_eq!(guard.counters().dropped(), 1);
    }

    #[test]
    fn queue_overflow_oversize_closed_disconnected_and_nonblocking_producers() {
        let limits = Limits {
            queue: 2,
            record: 8,
            ..Limits::default()
        };
        let (writer, mut guard) = channel_with_limits(limits);
        send(&writer, b"{}\n");
        send(&writer, b"{}\n");
        let before = Instant::now();
        for _ in 0..10_000 {
            send(&writer, b"{}\n");
        }
        assert!(before.elapsed() < Duration::from_secs(1));
        assert_eq!(guard.counters().dropped(), 10_000);
        let mut event = writer.make_writer();
        event.write_all(b"12345678").unwrap();
        event.write_all(b"\n").unwrap(); // newline must fit the same event cap.
        assert!(event.buffer.is_empty());
        event.flush().unwrap();
        drop(event);
        assert_eq!(guard.counters().dropped(), 10_001);
        assert!(guard.shutdown_with_budget(Duration::from_secs(2)));
        send(&writer, b"{}\n");
        assert_eq!(guard.counters().dropped(), 10_004); // two queued + closed.
        let (writer, mut guard) = channel_with_limits(limits);
        guard.rx = None;
        send(&writer, b"{}\n");
        assert_eq!(guard.counters().dropped(), 1);
    }

    #[test]
    fn writer_chunking_cap_and_missing_newline_do_not_submit_partial_records() {
        let limits = Limits {
            record: 12,
            ..Limits::default()
        };
        let (writer, guard) = channel_with_limits(limits);
        let mut event = writer.make_writer();
        for part in [b"{\"x\":".as_slice(), b"\"ok\"", b"}\n"] {
            event.write_all(part).unwrap();
        }
        assert!(event.buffer.capacity() <= limits.record);
        drop(event);
        assert_eq!(
            guard.rx.as_ref().unwrap().try_recv().unwrap(),
            b"{\"x\":\"ok\"}\n"
        );
        send(&writer, b"{\"bad\":");
        assert_eq!(guard.counters().dropped(), 1);
    }

    #[test]
    fn free_space_predicate_is_fail_closed_and_overflow_safe() {
        assert!(enough_space(Some(103), 100, 3));
        assert!(!enough_space(Some(102), 100, 3));
        assert!(!enough_space(None, 0, 0));
        assert!(!enough_space(Some(u64::MAX), u64::MAX, 1));
        let limits = Limits::default();
        assert_eq!(limits.record * limits.queue, 2 * 1024 * 1024);
        assert_eq!(
            limits.active * 2 + limits.archived + limits.compressed,
            128 * 1024 * 1024
        );
    }

    struct FailingWriter {
        bytes: Vec<u8>,
        left: usize,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.left == 0 {
                return Err(io::Error::from(io::ErrorKind::StorageFull));
            }
            let count = bytes.len().min(self.left);
            self.bytes.extend_from_slice(&bytes[..count]);
            self.left -= count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn compression_write_errors_and_finish_footer_cap_are_propagated_without_panic() {
        let source = b"{\"text\":\"small record buffered until finish\"}\n";
        let mut writer = FailingWriter {
            bytes: Vec::new(),
            left: 5,
        };
        assert!(compress(source.as_slice(), &mut writer, 1024, 1024).is_err());
        assert_eq!(writer.bytes.len(), 5);
        let full = compress(source.as_slice(), Vec::new(), 1024, 1024).unwrap();
        assert_eq!(full.inner[4] & 4, 4, "zstd frame must include checksum");
        let mut capped = Vec::new();
        assert!(compress(source.as_slice(), &mut capped, 1024, full.written - 1).is_err());
        assert!((capped.len() as u64) < full.written);
        assert!(compress(source.as_slice(), Vec::new(), 1, 1024).is_err());
    }

    #[test]
    fn partial_write_failure_restores_only_committed_prefix_and_reports_failed_rollback() {
        let prefix = b"{\"prefix\":1}\n";
        let mut writer = FailingWriter {
            bytes: prefix.to_vec(),
            left: 5,
        };
        let failure = write_or_rollback(&mut writer, b"{\"next\":2}\n", |writer| {
            writer.bytes.truncate(prefix.len());
            Ok(())
        })
        .unwrap_err();
        assert_eq!(failure.0.kind(), io::ErrorKind::StorageFull);
        assert!(!failure.1);
        assert_eq!(writer.bytes, prefix);
        writer.left = usize::MAX;
        write_or_rollback(&mut writer, b"{\"next\":2}\n", |_| Ok(())).unwrap();
        assert_eq!(writer.bytes, b"{\"prefix\":1}\n{\"next\":2}\n");
        writer.left = 1;
        let failure = write_or_rollback(&mut writer, b"{}\n", |_| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert!(failure.1);
    }

    #[test]
    fn shutdown_without_worker_is_immediate_and_has_no_filesystem_lifetime() {
        let (writer, mut guard) = trace_log_channel();
        send(&writer, b"{}\n");
        let before = Instant::now();
        assert!(guard.shutdown_with_budget(Duration::from_secs(2)));
        assert!(before.elapsed() < Duration::from_millis(100));
        assert_eq!(guard.counters().dropped(), 1);
        let (writer, guard) = trace_log_channel();
        send(&writer, b"{}\n");
        let before = Instant::now();
        drop(guard);
        assert!(before.elapsed() < Duration::from_millis(100));
        assert!(writer.shared.closing.load(Ordering::Acquire));
        assert_eq!(writer.shared.counters.dropped(), 1);
    }

    #[test]
    fn shutdown_budget_never_joins_a_blocked_worker_or_waits_again_in_drop() {
        let (_, mut guard) = trace_log_channel();
        let (release, blocked) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        guard.started = true;
        guard.done = Some(done);
        guard.worker = Some(thread::spawn(move || {
            let _ = blocked.recv();
            let _ = done_tx.send(());
            let _ = finished_tx.send(());
        }));
        let before = Instant::now();
        assert!(!guard.shutdown_with_budget(Duration::from_millis(10)));
        drop(guard);
        assert!(before.elapsed() < Duration::from_millis(250));
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn worker_drains_once_and_all_producer_worker_counters_share_the_same_arc() {
        let temp = tempfile::TempDir::new().unwrap();
        let (writer, mut guard) = trace_log_channel();
        assert!(!temp.path().join("logs").exists());
        assert!(Arc::ptr_eq(&writer.shared.counters, &guard.counters()));
        for number in 0..20 {
            send(&writer, format!("{{\"n\":{number}}}\n").as_bytes());
        }
        guard.start(temp.path()).unwrap();
        assert_eq!(
            guard.start(temp.path()).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(guard.shutdown_with_budget(Duration::from_secs(2)));
        let raw = std::fs::read(temp.path().join("logs/stream-trace/active.jsonl")).unwrap();
        assert_eq!(
            raw.split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            20
        );
        assert_eq!(guard.counters().dropped(), 0);
        assert_eq!(guard.counters().io_errors(), 0);
        send(&writer, b"{}\n");
        assert_eq!(guard.counters().dropped(), 1);
        assert!(guard.shutdown_with_budget(Duration::from_millis(0)));
    }

    #[test]
    fn unavailable_storage_counts_dropped_records_and_reports_typed_throttled_errors() {
        let temp = tempfile::TempDir::new().unwrap();
        let (writer, mut guard) = trace_log_channel();
        guard.start(&temp.path().join("missing-parent")).unwrap();
        for _ in 0..5 {
            send(&writer, b"{}\n");
        }
        assert!(guard.shutdown_with_budget(Duration::from_secs(2)));
        assert_eq!(guard.counters().dropped(), 5);
        assert_eq!(guard.counters().io_errors(), 1);
        let mut reporter = Reporter {
            counters: guard.counters(),
            warned: None,
            seen_drops: 0,
        };
        let failure = Failure::at(
            "test-stage",
            io::Error::other("private path must not be displayed"),
        );
        reporter.failure(&failure);
        let warned = reporter.warned;
        reporter.failure(&failure);
        assert_eq!(reporter.warned, warned);
        assert_eq!(guard.counters().io_errors(), 3);
        assert_eq!(
            error_kind(io::ErrorKind::PermissionDenied),
            "permission-denied"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn failed_space_probe_backs_off_io_while_continuing_to_drop_without_blocking() {
        let temp = tempfile::TempDir::new().unwrap();
        let limits = Limits {
            available: Some(None),
            ..Limits::default()
        };
        let (writer, mut guard) = channel_with_limits(limits);
        for _ in 0..20 {
            send(&writer, b"{}\n");
        }
        guard.start(temp.path()).unwrap();
        assert!(guard.shutdown_with_budget(Duration::from_secs(2)));
        assert_eq!(guard.counters().dropped(), 20);
        assert_eq!(guard.counters().io_errors(), 1);
        assert_eq!(
            std::fs::metadata(temp.path().join("logs/stream-trace/active.jsonl"))
                .unwrap()
                .len(),
            0
        );
    }
}
