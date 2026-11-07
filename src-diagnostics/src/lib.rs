use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const MAX_LOG_CHUNK_BYTES: usize = 8 * 1024;
pub const MAX_LOG_RECORD_BYTES: usize = 64 * 1024;
const DEFAULT_QUEUE_CAPACITY: usize = 2_048;
const DEFAULT_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticLogRecord {
    /// Event time in UTC. Missing for legacy records whose source timezone is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_utc: Option<String>,
    pub timestamp_local: String,
    pub monotonic_ms: u64,
    pub level: String,
    pub target: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timestamp: Option<String>,
    /// Time this application imported a legacy line; it is not the event time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_at_utc: Option<String>,
}

impl DiagnosticLogRecord {
    pub fn now(level: impl Into<String>, target: impl Into<String>, message: impl Into<String>) -> Self {
        let utc = Utc::now();
        Self {
            timestamp_utc: Some(utc.to_rfc3339_opts(SecondsFormat::Millis, true)),
            timestamp_local: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false),
            monotonic_ms: monotonic_ms(),
            level: level.into(),
            target: target.into(),
            message: message.into(),
            source_timestamp: None,
            imported_at_utc: None,
        }
    }

    pub fn imported_legacy(
        level: impl Into<String>,
        line: impl Into<String>,
        source_timestamp: Option<String>,
    ) -> Self {
        let imported_at = Utc::now();
        let event_timestamp_utc = source_timestamp
            .as_deref()
            .and_then(|source| DateTime::parse_from_rfc3339(source).ok())
            .map(|timestamp| timestamp.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::Millis, true));
        Self {
            timestamp_utc: event_timestamp_utc,
            timestamp_local: source_timestamp.clone().unwrap_or_default(),
            monotonic_ms: monotonic_ms(),
            level: level.into(),
            target: "legacy.transport".to_owned(),
            message: line.into(),
            source_timestamp,
            imported_at_utc: Some(imported_at.to_rfc3339_opts(SecondsFormat::Millis, true)),
        }
    }

    /// Historical imports take their event time only from an explicit, parseable source offset.
    /// This also recognizes old persisted records whose `timestamp_utc` was incorrectly set to
    /// their import time.
    pub fn event_timestamp_utc(&self) -> Option<DateTime<Utc>> {
        if let Some(source_timestamp) = self.source_timestamp.as_deref() {
            return DateTime::parse_from_rfc3339(source_timestamp)
                .ok()
                .map(|timestamp| timestamp.with_timezone(&Utc));
        }
        self.timestamp_utc
            .as_deref()
            .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
            .map(|timestamp| timestamp.with_timezone(&Utc))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogTimeRange {
    pub start_utc: String,
    pub end_utc: String,
}

impl LogTimeRange {
    pub fn new(start_utc: &str, end_utc: &str) -> Result<Self, String> {
        let start = DateTime::parse_from_rfc3339(start_utc)
            .map_err(|_| "range start must be an RFC3339 timestamp with an explicit timezone".to_owned())?
            .with_timezone(&Utc);
        let end = DateTime::parse_from_rfc3339(end_utc)
            .map_err(|_| "range end must be an RFC3339 timestamp with an explicit timezone".to_owned())?
            .with_timezone(&Utc);
        if start >= end {
            return Err("range start must be earlier than range end".to_owned());
        }
        Ok(Self {
            start_utc: start.to_rfc3339_opts(SecondsFormat::Millis, true),
            end_utc: end.to_rfc3339_opts(SecondsFormat::Millis, true),
        })
    }

    fn bounds(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        let start = DateTime::parse_from_rfc3339(&self.start_utc)
            .expect("LogTimeRange is validated")
            .with_timezone(&Utc);
        let end = DateTime::parse_from_rfc3339(&self.end_utc)
            .expect("LogTimeRange is validated")
            .with_timezone(&Utc);
        (start, end)
    }

}

#[derive(Clone, Debug)]
pub struct RetentionPolicy {
    pub max_age: Duration,
    pub max_total_bytes: u64,
    pub segment_bytes: u64,
    pub queue_capacity: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(7 * 24 * 60 * 60),
            max_total_bytes: 256 * 1024 * 1024,
            segment_bytes: DEFAULT_SEGMENT_BYTES,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogSnapshotMetadata {
    pub snapshot_id: Uuid,
    pub total_bytes: u64,
    pub file_count: usize,
    pub coverage_start_utc: Option<String>,
    pub coverage_end_utc: Option<String>,
    pub retention_truncated: bool,
    pub dropped_records: u64,
    pub writer_errors: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_time_range: Option<LogTimeRange>,
    #[serde(default)]
    pub retained_record_count: u64,
    #[serde(default)]
    pub exported_record_count: u64,
    #[serde(default)]
    pub excluded_by_time_range_count: u64,
    #[serde(default)]
    pub unknown_timestamp_count: u64,
    #[serde(default)]
    pub retained_coverage_start_utc: Option<String>,
    #[serde(default)]
    pub retained_coverage_end_utc: Option<String>,
    #[serde(default)]
    pub requested_range_extends_beyond_retained_coverage: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogExportChunk {
    pub snapshot_id: Uuid,
    pub offset: u64,
    pub next_offset: u64,
    pub bytes: Vec<u8>,
    pub eof: bool,
    pub sha256: Option<String>,
}

enum WriterMessage {
    Record(DiagnosticLogRecord),
    Flush(mpsc::SyncSender<Result<(), String>>),
}

struct StoreInner {
    root: PathBuf,
    sender: SyncSender<WriterMessage>,
    dropped_records: Arc<AtomicU64>,
    writer_errors: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct PersistentLogStore {
    inner: Arc<StoreInner>,
}

impl PersistentLogStore {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        Self::open_with_policy(path, RetentionPolicy::default())
    }

    pub fn open_with_policy(path: impl Into<PathBuf>, policy: RetentionPolicy) -> io::Result<Self> {
        if policy.max_age.is_zero()
            || policy.max_total_bytes == 0
            || policy.segment_bytes == 0
            || policy.segment_bytes > policy.max_total_bytes
            || policy.queue_capacity == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid diagnostic retention policy",
            ));
        }
        let root = path.into();
        fs::create_dir_all(&root)?;
        prune_logs(&root, &policy, None)?;
        let writer_state = WriterState::open(root.clone(), policy.clone())?;
        let (sender, receiver) = mpsc::sync_channel(policy.queue_capacity);
        let dropped_records = Arc::new(AtomicU64::new(0));
        let writer_errors = Arc::new(AtomicU64::new(0));
        let errors_for_worker = Arc::clone(&writer_errors);
        thread::Builder::new()
            .name("shockingvrc-diagnostic-log-writer".to_owned())
            .spawn(move || writer_loop(writer_state, receiver, errors_for_worker))?;
        Ok(Self {
            inner: Arc::new(StoreInner {
                root,
                sender,
                dropped_records,
                writer_errors,
            }),
        })
    }

    /// Nonblocking append. `false` means the bounded queue was full or the writer stopped.
    pub fn append(&self, record: DiagnosticLogRecord) -> bool {
        match self.inner.sender.try_send(WriterMessage::Record(record)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.inner.dropped_records.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.inner.writer_errors.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Flush records already queued before creating an export snapshot.
    pub fn flush(&self) -> io::Result<()> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.inner
            .sender
            .send(WriterMessage::Flush(sender))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "diagnostic writer stopped"))?;
        receiver
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "diagnostic writer stopped"))?
            .map_err(io::Error::other)
    }

    pub fn snapshot(&self) -> io::Result<LogSnapshot> {
        self.snapshot_filtered(None)
    }

    /// Capture a stable snapshot, filtering event timestamps on the writer's side. A bounded
    /// line buffer is used while scanning retained JSONL; selected rows are spooled to a private
    /// snapshot file so chunk reads never need to hold the retained history in memory.
    pub fn snapshot_filtered(&self, range: Option<&LogTimeRange>) -> io::Result<LogSnapshot> {
        self.flush()?;
        let mut paths = log_paths(&self.inner.root)?;
        paths.sort();
        let lengths = paths
            .iter()
            .map(|path| File::open(path)?.metadata().map(|metadata| metadata.len()))
            .collect::<io::Result<Vec<_>>>()?;
        let snapshot_id = Uuid::new_v4();
        let snapshots_dir = self.inner.root.join(".snapshots");
        fs::create_dir_all(&snapshots_dir)?;
        let temporary_path = Some(snapshots_dir.join(format!("snapshot-{snapshot_id}.jsonl")));
        let mut temporary_guard = SnapshotTempGuard::new(temporary_path.clone());
        let mut filtered_file = match &temporary_path {
            Some(path) => Some(OpenOptions::new().write(true).create_new(true).open(path)?),
            None => None,
        };
        let stats = match scan_snapshot_records(&paths, &lengths, range, filtered_file.as_mut()) {
            Ok(stats) => stats,
            Err(error) => return Err(error),
        };
        let mut files = Vec::new();
        let total_bytes;
        let file_count;
        if let (Some(path), Some(mut file)) = (temporary_path.as_ref(), filtered_file.take()) {
            file.flush()?;
            file.sync_all()?;
            let length = file.metadata()?.len();
            drop(file);
            if length > 0 {
                files.push(SnapshotFile {
                    file: File::open(path)?,
                    length,
                });
            }
            total_bytes = length;
            file_count = if range.is_some() {
                usize::from(length > 0)
            } else {
                lengths.iter().filter(|length| **length > 0).count()
            };
        } else {
            unreachable!("snapshot spool path is always created")
        }
        let requested_range_extends_beyond_retained_coverage = range.is_some_and(|requested| {
            match (&stats.retained_start, &stats.retained_end) {
                (Some(coverage_start), Some(coverage_end)) => {
                    let (start, end) = requested.bounds();
                    start < *coverage_start || end > *coverage_end
                }
                _ => true,
            }
        });
        let metadata = LogSnapshotMetadata {
            snapshot_id,
            total_bytes,
            file_count,
            coverage_start_utc: stats.exported_start.map(format_utc),
            coverage_end_utc: stats.exported_end.map(format_utc),
            retention_truncated: self.inner.root.join("retention-truncated").exists(),
            dropped_records: self.inner.dropped_records.load(Ordering::Relaxed),
            writer_errors: self.inner.writer_errors.load(Ordering::Relaxed),
            requested_time_range: range.cloned(),
            retained_record_count: stats.retained_records,
            exported_record_count: stats.exported_records,
            excluded_by_time_range_count: stats.excluded_records,
            unknown_timestamp_count: stats.unknown_timestamps,
            retained_coverage_start_utc: stats.retained_start.map(format_utc),
            retained_coverage_end_utc: stats.retained_end.map(format_utc),
            requested_range_extends_beyond_retained_coverage,
        };
        temporary_guard.disarm();
        Ok(LogSnapshot {
            metadata,
            files,
            temporary_path,
            hasher: Sha256::new(),
            next_offset: 0,
            last_chunk: None,
            completed_hash: None,
        })
    }

    /// Return the newest valid JSONL records in chronological order without loading the
    /// retained history into memory. Each inspected file is bounded to a 512 KiB tail.
    pub fn tail_records(&self, limit: usize) -> io::Result<Vec<DiagnosticLogRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.flush()?;
        let mut paths = log_paths(&self.inner.root)?;
        paths.sort();
        // Visit newest files and records first. Keep this deque newest-first;
        // when it exceeds the limit, discard the newly visited older record.
        let mut tail = std::collections::VecDeque::with_capacity(limit.min(512));
        for path in paths.into_iter().rev() {
            let mut file = File::open(path)?;
            let length = file.metadata()?.len();
            let start = length.saturating_sub(512 * 1024);
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::with_capacity((length - start) as usize);
            file.read_to_end(&mut bytes)?;
            let text = String::from_utf8_lossy(&bytes);
            let mut lines = text.lines().collect::<Vec<_>>();
            if start > 0 && !lines.is_empty() {
                lines.remove(0);
            }
            for line in lines.into_iter().rev() {
                if let Ok(record) = serde_json::from_str::<DiagnosticLogRecord>(line) {
                    tail.push_back(record);
                    if tail.len() > limit {
                        tail.pop_back();
                    }
                }
            }
            if tail.len() >= limit {
                break;
            }
        }
        let mut records = tail.into_iter().collect::<Vec<_>>();
        records.reverse();
        Ok(records)
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }
}

struct SnapshotFile {
    file: File,
    length: u64,
}

pub struct LogSnapshot {
    metadata: LogSnapshotMetadata,
    files: Vec<SnapshotFile>,
    temporary_path: Option<PathBuf>,
    hasher: Sha256,
    next_offset: u64,
    last_chunk: Option<LogExportChunk>,
    completed_hash: Option<String>,
}

impl Drop for LogSnapshot {
    fn drop(&mut self) {
        self.files.clear();
        if let Some(path) = &self.temporary_path {
            let _ = fs::remove_file(path);
        }
    }
}

struct SnapshotTempGuard(Option<PathBuf>);

impl SnapshotTempGuard {
    fn new(path: Option<PathBuf>) -> Self {
        Self(path)
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for SnapshotTempGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

impl LogSnapshot {
    pub fn metadata(&self) -> &LogSnapshotMetadata {
        &self.metadata
    }

    pub fn next_chunk(&mut self, offset: u64, requested_bytes: usize) -> io::Result<LogExportChunk> {
        if !(1..=MAX_LOG_CHUNK_BYTES).contains(&requested_bytes) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "log chunk size is out of range"));
        }
        if let Some(previous) = &self.last_chunk
            && previous.offset == offset
        {
            return Ok(previous.clone());
        }
        if offset != self.next_offset {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log chunk offset is out of sequence",
            ));
        }
        if offset > self.metadata.total_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log chunk offset exceeds the snapshot",
            ));
        }

        let count = (self.metadata.total_bytes - offset).min(requested_bytes as u64) as usize;
        let mut bytes = vec![0; count];
        self.read_at(offset, &mut bytes)?;
        self.hasher.update(&bytes);
        let next_offset = offset + count as u64;
        self.next_offset = next_offset;
        let eof = next_offset == self.metadata.total_bytes;
        if eof && self.completed_hash.is_none() {
            self.completed_hash = Some(hex(&self.hasher.clone().finalize()));
        }
        let chunk = LogExportChunk {
            snapshot_id: self.metadata.snapshot_id,
            offset,
            next_offset,
            bytes,
            eof,
            sha256: if eof { self.completed_hash.clone() } else { None },
        };
        self.last_chunk = Some(chunk.clone());
        Ok(chunk)
    }

    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        let mut global_start = 0_u64;
        let mut remaining = output;
        for snapshot_file in &mut self.files {
            let global_end = global_start + snapshot_file.length;
            if offset < global_end && !remaining.is_empty() {
                let within_file = offset.saturating_sub(global_start);
                let available = (snapshot_file.length - within_file).min(remaining.len() as u64) as usize;
                snapshot_file.file.seek(SeekFrom::Start(within_file))?;
                snapshot_file.file.read_exact(&mut remaining[..available])?;
                remaining = &mut remaining[available..];
            }
            global_start = global_end;
        }
        if remaining.is_empty() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "diagnostic snapshot changed while reading",
            ))
        }
    }
}

fn writer_loop(mut state: WriterState, receiver: Receiver<WriterMessage>, writer_errors: Arc<AtomicU64>) {
    while let Ok(message) = receiver.recv() {
        match message {
            WriterMessage::Record(mut record) => {
                bound_record(&mut record);
                if state.append(&record).is_err() {
                    writer_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            WriterMessage::Flush(reply) => {
                let result = state.flush().map_err(|error| error.to_string());
                if result.is_err() {
                    writer_errors.fetch_add(1, Ordering::Relaxed);
                }
                let _ = reply.send(result);
            }
        }
    }
    let _ = state.flush();
}

struct WriterState {
    root: PathBuf,
    policy: RetentionPolicy,
    date: String,
    segment: u32,
    file: File,
    length: u64,
}

impl WriterState {
    fn open(root: PathBuf, policy: RetentionPolicy) -> io::Result<Self> {
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let latest = latest_segment(&root, &date)?;
        let (segment, path, length) = match latest {
            Some((segment, path, length)) if length < policy.segment_bytes => (segment, path, length),
            Some((segment, _, _)) => {
                let next = segment.saturating_add(1);
                (next, root.join(segment_name(&date, next)), 0)
            }
            None => (0, root.join(segment_name(&date, 0)), 0),
        };
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            root,
            policy,
            date,
            segment,
            file,
            length,
        })
    }

    fn append(&mut self, record: &DiagnosticLogRecord) -> io::Result<()> {
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let mut bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        if bytes.len() > MAX_LOG_RECORD_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "diagnostic record exceeds bound"));
        }
        bytes.push(b'\n');
        if bytes.len() as u64 > self.policy.max_total_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "diagnostic record exceeds retention limit",
            ));
        }
        let rotated = date != self.date || self.length.saturating_add(bytes.len() as u64) > self.policy.segment_bytes;
        if rotated {
            self.rotate(&date)?;
        }
        self.file.write_all(&bytes)?;
        self.length = self.length.saturating_add(bytes.len() as u64);
        prune_logs(&self.root, &self.policy, Some(&self.current_path()))?;
        Ok(())
    }

    fn rotate(&mut self, date: &str) -> io::Result<()> {
        self.file.flush()?;
        if date == self.date {
            self.segment = self.segment.saturating_add(1);
        } else {
            self.date = date.to_owned();
            self.segment = 0;
        }
        let path = self.current_path();
        self.file = OpenOptions::new().create(true).append(true).open(path)?;
        self.length = self.file.metadata()?.len();
        Ok(())
    }

    fn current_path(&self) -> PathBuf {
        self.root.join(segment_name(&self.date, self.segment))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn latest_segment(root: &Path, today: &str) -> io::Result<Option<(u32, PathBuf, u64)>> {
    let mut matches = log_paths(root)?
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let prefix = format!("diagnostics-{today}-");
            let index = name
                .strip_prefix(&prefix)?
                .strip_suffix(".jsonl")?
                .parse::<u32>()
                .ok()?;
            let length = path.metadata().ok()?.len();
            Some((index, path, length))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(index, _, _)| *index);
    Ok(matches.into_iter().next_back())
}

fn segment_name(date: &str, segment: u32) -> String {
    format!("diagnostics-{date}-{segment:04}.jsonl")
}

fn log_paths(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "jsonl"))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn prune_logs(root: &Path, policy: &RetentionPolicy, active: Option<&Path>) -> io::Result<()> {
    let now = SystemTime::now();
    let cutoff = now.checked_sub(policy.max_age).unwrap_or(SystemTime::UNIX_EPOCH);
    let mut entries = log_paths(root)?
        .into_iter()
        .filter_map(|path| {
            let metadata = fs::metadata(&path).ok()?;
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            Some((path, metadata.len(), modified))
        })
        .collect::<Vec<_>>();
    let mut removed = false;
    for (path, _, modified) in &entries {
        if *modified < cutoff && active != Some(path.as_path()) && fs::remove_file(path).is_ok() {
            removed = true;
        }
    }
    entries.retain(|(path, _, modified)| !(modified < &cutoff && active != Some(path.as_path()) && !path.exists()));
    let mut total: u64 = entries.iter().map(|(_, length, _)| *length).sum();
    for (path, length, _) in entries.iter() {
        if total <= policy.max_total_bytes {
            break;
        }
        if active == Some(path.as_path()) {
            continue;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(*length);
            removed = true;
        }
    }
    if removed {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("retention-truncated"))?
            .write_all(b"retention limits removed older log segments\n")?;
    }
    Ok(())
}

#[derive(Default)]
struct SnapshotScanStats {
    retained_records: u64,
    exported_records: u64,
    excluded_records: u64,
    unknown_timestamps: u64,
    retained_start: Option<DateTime<Utc>>,
    retained_end: Option<DateTime<Utc>>,
    exported_start: Option<DateTime<Utc>>,
    exported_end: Option<DateTime<Utc>>,
}

fn scan_snapshot_records(
    paths: &[PathBuf],
    lengths: &[u64],
    range: Option<&LogTimeRange>,
    mut output: Option<&mut File>,
) -> io::Result<SnapshotScanStats> {
    let mut stats = SnapshotScanStats::default();
    let range_bounds = range.map(LogTimeRange::bounds);
    for (path, length) in paths.iter().zip(lengths.iter().copied()) {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file).take(length);
        let mut line = Vec::new();
        loop {
            let line_length = read_bounded_line(&mut reader, &mut line)?;
            if line_length == 0 {
                break;
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                line.clear();
                continue;
            }
            stats.retained_records = stats.retained_records.saturating_add(1);
            let record = serde_json::from_slice::<DiagnosticLogRecord>(&line).ok();
            let event_time = record.as_ref().and_then(DiagnosticLogRecord::event_timestamp_utc);
            if let Some(timestamp) = event_time {
                set_time_bounds(&mut stats.retained_start, &mut stats.retained_end, timestamp);
            } else {
                stats.unknown_timestamps = stats.unknown_timestamps.saturating_add(1);
            }
            let selected = match (range_bounds, event_time) {
                (Some((start, end)), Some(timestamp)) => start <= timestamp && timestamp < end,
                (Some(_), None) => false,
                (None, _) => true,
            };
            if selected {
                stats.exported_records = stats.exported_records.saturating_add(1);
                if let Some(timestamp) = event_time {
                    set_time_bounds(&mut stats.exported_start, &mut stats.exported_end, timestamp);
                }
                if let Some(output) = output.as_deref_mut() {
                    let normalized = normalize_legacy_record(&line, record.as_ref())?;
                    output.write_all(&normalized)?;
                    if !normalized.ends_with(b"\n") {
                        output.write_all(b"\n")?;
                    }
                }
            } else if event_time.is_some() {
                stats.excluded_records = stats.excluded_records.saturating_add(1);
            }
            line.clear();
        }
    }
    Ok(stats)
}

fn normalize_legacy_record(line: &[u8], record: Option<&DiagnosticLogRecord>) -> io::Result<Vec<u8>> {
    let Some(record) = record.filter(|record| record.source_timestamp.is_some()) else {
        return Ok(line.to_vec());
    };
    let mut normalized = record.clone();
    normalized.timestamp_utc = record.event_timestamp_utc().map(format_utc);
    if normalized.timestamp_utc == record.timestamp_utc {
        return Ok(line.to_vec());
    }
    serde_json::to_vec(&normalized).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn read_bounded_line<R: BufRead>(reader: &mut R, output: &mut Vec<u8>) -> io::Result<usize> {
    output.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(output.len());
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if output.len().saturating_add(take) > MAX_LOG_RECORD_BYTES + 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "diagnostic record exceeds the bounded snapshot line limit",
            ));
        }
        let has_newline = available.get(take.saturating_sub(1)) == Some(&b'\n');
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if has_newline {
            return Ok(output.len());
        }
    }
}

fn set_time_bounds(
    start: &mut Option<DateTime<Utc>>,
    end: &mut Option<DateTime<Utc>>,
    timestamp: DateTime<Utc>,
) {
    if start.is_none_or(|current| timestamp < current) {
        *start = Some(timestamp);
    }
    if end.is_none_or(|current| timestamp > current) {
        *end = Some(timestamp);
    }
}

fn format_utc(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn bound_record(record: &mut DiagnosticLogRecord) {
    record.level = record.level.chars().take(16).collect();
    record.target = record.target.chars().take(256).collect();
    record.message = record.message.chars().take(2_000).collect();
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn monotonic_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shockingvrc-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(message: &str) -> DiagnosticLogRecord {
        DiagnosticLogRecord::now("INFO", "test", message)
    }

    #[test]
    fn snapshot_is_stable_and_replayed_chunk_is_identical() {
        let dir = temp_dir("diagnostics-snapshot");
        let store = PersistentLogStore::open(&dir).unwrap();
        assert!(store.append(record("before export")));
        let mut snapshot = store.snapshot().unwrap();
        let frozen_bytes = snapshot.metadata().total_bytes;
        assert!(store.append(record("after export")));
        store.flush().unwrap();

        let first = snapshot.next_chunk(0, MAX_LOG_CHUNK_BYTES).unwrap();
        assert_eq!(first, snapshot.next_chunk(0, MAX_LOG_CHUNK_BYTES).unwrap());
        assert!(first.eof);
        assert_eq!(first.next_offset, frozen_bytes);
        assert!(first.sha256.is_some());
        let text = String::from_utf8(first.bytes).unwrap();
        assert!(text.contains("before export"));
        assert!(!text.contains("after export"));
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retention_rotation_is_bounded_and_discloses_pruning() {
        let dir = temp_dir("diagnostics-retention");
        let policy = RetentionPolicy {
            max_age: Duration::from_secs(7 * 24 * 60 * 60),
            max_total_bytes: 1_800,
            segment_bytes: 700,
            queue_capacity: 32,
        };
        let store = PersistentLogStore::open_with_policy(&dir, policy.clone()).unwrap();
        for index in 0..18 {
            assert!(store.append(record(&format!("{}-{index}", "x".repeat(180)))));
        }
        store.flush().unwrap();
        let snapshot = store.snapshot().unwrap();
        assert!(snapshot.metadata().retention_truncated);
        assert!(snapshot.metadata().total_bytes <= policy.max_total_bytes);
        assert!(snapshot.metadata().file_count > 1);
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chunk_limit_and_sequence_are_enforced() {
        let dir = temp_dir("diagnostics-chunks");
        let store = PersistentLogStore::open(&dir).unwrap();
        store.append(record("chunk bounds"));
        let mut snapshot = store.snapshot().unwrap();
        assert!(snapshot.next_chunk(0, MAX_LOG_CHUNK_BYTES + 1).is_err());
        assert!(snapshot.next_chunk(1, 4).is_err());
        assert!(snapshot.next_chunk(0, 4).is_ok());
        assert!(snapshot.next_chunk(4, 4).is_ok());
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tail_records_returns_latest_records_in_chronological_order() {
        let dir = temp_dir("diagnostics-tail");
        let store = PersistentLogStore::open_with_policy(
            &dir,
            RetentionPolicy {
                max_age: Duration::from_secs(7 * 24 * 60 * 60),
                max_total_bytes: 64 * 1024,
                segment_bytes: 64 * 1024,
                queue_capacity: 32,
            },
        )
        .unwrap();
        for index in 0..8 {
            assert!(store.append(record(&format!("record-{index}"))));
        }
        let tail = store.tail_records(3).unwrap();
        assert_eq!(
            tail.iter().map(|record| record.message.as_str()).collect::<Vec<_>>(),
            ["record-5", "record-6", "record-7"]
        );
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn jsonl_records_carry_original_wall_and_monotonic_timestamps() {
        let dir = temp_dir("diagnostics-fields");
        let store = PersistentLogStore::open(&dir).unwrap();
        store.append(record("heartbeat acknowledged"));
        store.flush().unwrap();
        let mut snapshot = store.snapshot().unwrap();
        let chunk = snapshot.next_chunk(0, MAX_LOG_CHUNK_BYTES).unwrap();
        let saved: DiagnosticLogRecord =
            serde_json::from_slice(chunk.bytes.split(|byte| *byte == b'\n').next().unwrap()).unwrap();
        assert!(DateTime::parse_from_rfc3339(saved.timestamp_utc.as_deref().unwrap()).is_ok());
        assert!(DateTime::parse_from_rfc3339(&saved.timestamp_local).is_ok());
        assert!(saved.monotonic_ms <= monotonic_ms());
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_import_keeps_ingestion_separate_and_only_normalizes_explicit_offsets() {
        let known = DiagnosticLogRecord::imported_legacy(
            "INFO",
            "legacy event",
            Some("2026-10-17T09:30:00.000+04:00".to_owned()),
        );
        assert_eq!(known.source_timestamp.as_deref(), Some("2026-10-17T09:30:00.000+04:00"));
        assert_eq!(known.timestamp_utc.as_deref(), Some("2026-10-17T05:30:00.000Z"));
        assert!(known.imported_at_utc.is_some());

        let ambiguous = DiagnosticLogRecord::imported_legacy(
            "INFO",
            "ambiguous legacy event",
            Some("2026-10-17 09:30:00".to_owned()),
        );
        assert_eq!(ambiguous.source_timestamp.as_deref(), Some("2026-10-17 09:30:00"));
        assert!(ambiguous.timestamp_utc.is_none());
        assert!(ambiguous.imported_at_utc.is_some());
        let malformed = DiagnosticLogRecord::imported_legacy(
            "INFO",
            "malformed legacy event",
            Some("not-an-rfc3339-time".to_owned()),
        );
        assert!(malformed.timestamp_utc.is_none());
        assert_eq!(malformed.source_timestamp.as_deref(), Some("not-an-rfc3339-time"));

        let old_persisted = serde_json::from_value::<DiagnosticLogRecord>(serde_json::json!({
            "timestamp_utc":"2026-10-24T10:00:00.000Z",
            "timestamp_local":"2026-10-24T14:00:00.000+04:00",
            "monotonic_ms":42,
            "level":"INFO",
            "target":"legacy.transport",
            "message":"historical record",
            "source_timestamp":"2026-10-17 09:30:00"
        }))
        .unwrap();
        assert!(old_persisted.event_timestamp_utc().is_none(), "old import time is not the legacy event time");

        let exact_source = format!("legacy-ts-{}", "x".repeat(300));
        let mut long_legacy = DiagnosticLogRecord::imported_legacy("INFO", "line", Some(exact_source.clone()));
        bound_record(&mut long_legacy);
        assert_eq!(long_legacy.source_timestamp.as_deref(), Some(exact_source.as_str()));
    }

    #[test]
    fn all_retained_snapshot_repairs_old_import_time_without_mutating_history() {
        let dir = temp_dir("diagnostics-legacy-snapshot");
        let store = PersistentLogStore::open(&dir).unwrap();
        let mut known = DiagnosticLogRecord::imported_legacy(
            "INFO",
            "known legacy event",
            Some("2026-10-17T09:30:00.000+04:00".to_owned()),
        );
        known.timestamp_utc = Some("2026-10-24T09:30:00.000Z".to_owned());
        known.imported_at_utc = None;
        let mut ambiguous = DiagnosticLogRecord::imported_legacy(
            "INFO",
            "ambiguous legacy event",
            Some("2026-10-17 09:30:00".to_owned()),
        );
        ambiguous.timestamp_utc = Some("2026-10-24T09:30:00.000Z".to_owned());
        ambiguous.imported_at_utc = None;
        assert!(store.append(known));
        assert!(store.append(ambiguous));
        store.flush().unwrap();

        let source_path = log_paths(&dir).unwrap().into_iter().next().unwrap();
        let persisted_before = fs::read_to_string(&source_path).unwrap();
        let mut snapshot = store.snapshot().unwrap();
        let chunk = snapshot.next_chunk(0, MAX_LOG_CHUNK_BYTES).unwrap();
        let records = chunk
            .bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<DiagnosticLogRecord>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].source_timestamp.as_deref(), Some("2026-10-17T09:30:00.000+04:00"));
        assert_eq!(records[0].timestamp_utc.as_deref(), Some("2026-10-17T05:30:00.000Z"));
        assert_eq!(records[1].source_timestamp.as_deref(), Some("2026-10-17 09:30:00"));
        assert!(records[1].timestamp_utc.is_none());
        assert!(records.iter().all(|record| record.imported_at_utc.is_none()));
        assert_eq!(snapshot.metadata().unknown_timestamp_count, 1);
        assert_eq!(fs::read_to_string(&source_path).unwrap(), persisted_before);

        drop(snapshot);
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn filtered_snapshot_uses_half_open_utc_bounds_and_reports_unknown_legacy_times() {
        let dir = temp_dir("diagnostics-filtered-snapshot");
        let store = PersistentLogStore::open_with_policy(
            &dir,
            RetentionPolicy {
                max_age: Duration::from_secs(7 * 24 * 60 * 60),
                max_total_bytes: 64 * 1024,
                segment_bytes: 64 * 1024,
                queue_capacity: 16,
            },
        )
        .unwrap();
        let make = |timestamp_utc: Option<&str>, message: &str| DiagnosticLogRecord {
            timestamp_utc: timestamp_utc.map(str::to_owned),
            timestamp_local: timestamp_utc.unwrap_or("unknown").to_owned(),
            monotonic_ms: 0,
            level: "INFO".to_owned(),
            target: "test".to_owned(),
            message: message.to_owned(),
            source_timestamp: None,
            imported_at_utc: None,
        };
        for record in [
            make(Some("2026-10-24T09:59:59.999Z"), "before"),
            make(Some("2026-10-24T10:00:00.000Z"), "at-start"),
            make(Some("2026-10-24T10:59:59.999Z"), "inside"),
            make(Some("2026-10-24T11:00:00.000Z"), "at-end"),
            DiagnosticLogRecord::imported_legacy(
                "INFO",
                "unknown-time",
                Some("2026-10-24 10:30:00".to_owned()),
            ),
        ] {
            assert!(store.append(record));
        }
        let range = LogTimeRange::new("2026-10-24T10:00:00Z", "2026-10-24T11:00:00Z").unwrap();
        let mut snapshot = store.snapshot_filtered(Some(&range)).unwrap();
        let metadata = snapshot.metadata().clone();
        assert_eq!(metadata.retained_record_count, 5);
        assert_eq!(metadata.exported_record_count, 2);
        assert_eq!(metadata.excluded_by_time_range_count, 2);
        assert_eq!(metadata.unknown_timestamp_count, 1);
        let requested = metadata.requested_time_range.as_ref().unwrap();
        assert_eq!(requested.start_utc, "2026-10-24T10:00:00.000Z");
        assert_eq!(requested.end_utc, "2026-10-24T11:00:00.000Z");
        let chunk = snapshot.next_chunk(0, MAX_LOG_CHUNK_BYTES).unwrap();
        let body = String::from_utf8(chunk.bytes).unwrap();
        assert!(body.contains("at-start"));
        assert!(body.contains("inside"));
        assert!(!body.contains("before"));
        assert!(!body.contains("at-end"));
        assert!(!body.contains("unknown-time"));
        let filtered_snapshot_path = dir.join(".snapshots").join(format!("snapshot-{}.jsonl", metadata.snapshot_id));
        assert!(filtered_snapshot_path.exists());
        drop(snapshot);
        assert!(!filtered_snapshot_path.exists(), "cancel/drop releases the temporary filtered snapshot");

        let empty_range = LogTimeRange::new("2026-10-24T12:00:00Z", "2026-10-24T13:00:00Z").unwrap();
        let mut empty_snapshot = store.snapshot_filtered(Some(&empty_range)).unwrap();
        let empty_metadata = empty_snapshot.metadata();
        assert_eq!(empty_metadata.retained_record_count, 5);
        assert_eq!(empty_metadata.exported_record_count, 0);
        assert_eq!(empty_metadata.excluded_by_time_range_count, 4);
        assert_eq!(empty_metadata.unknown_timestamp_count, 1);
        assert!(empty_metadata.coverage_start_utc.is_none());
        assert!(empty_metadata.requested_range_extends_beyond_retained_coverage);
        let empty_chunk = empty_snapshot.next_chunk(0, MAX_LOG_CHUNK_BYTES).unwrap();
        assert!(empty_chunk.eof);
        assert!(empty_chunk.bytes.is_empty());
        drop(empty_snapshot);
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn range_rejects_reversed_or_timezone_free_bounds() {
        assert!(LogTimeRange::new("2026-10-24T11:00:00Z", "2026-10-24T10:00:00Z").is_err());
        assert!(LogTimeRange::new("2026-10-24T10:00:00", "2026-10-24T11:00:00Z").is_err());
    }

    #[test]
    fn records_remain_available_after_store_restart() {
        let dir = temp_dir("diagnostics-restart");
        {
            let store = PersistentLogStore::open(&dir).unwrap();
            assert!(store.append(record("before restart")));
            store.flush().unwrap();
        }

        let reopened = PersistentLogStore::open(&dir).unwrap();
        let tail = reopened.tail_records(10).unwrap();
        assert!(tail.iter().any(|record| record.message == "before restart"));
        drop(reopened);
        fs::remove_dir_all(dir).unwrap();
    }
}
