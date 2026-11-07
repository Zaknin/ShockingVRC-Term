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

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const MAX_LOG_CHUNK_BYTES: usize = 8 * 1024;
pub const MAX_LOG_RECORD_BYTES: usize = 64 * 1024;
const DEFAULT_QUEUE_CAPACITY: usize = 2_048;
const DEFAULT_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticLogRecord {
    pub timestamp_utc: String,
    pub timestamp_local: String,
    pub monotonic_ms: u64,
    pub level: String,
    pub target: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timestamp: Option<String>,
}

impl DiagnosticLogRecord {
    pub fn now(level: impl Into<String>, target: impl Into<String>, message: impl Into<String>) -> Self {
        let utc = Utc::now();
        Self {
            timestamp_utc: utc.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            timestamp_local: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false),
            monotonic_ms: monotonic_ms(),
            level: level.into(),
            target: target.into(),
            message: message.into(),
            source_timestamp: None,
        }
    }

    pub fn imported_legacy(
        level: impl Into<String>,
        line: impl Into<String>,
        source_timestamp: Option<String>,
    ) -> Self {
        let mut record = Self::now(level, "legacy.transport", line);
        record.source_timestamp = source_timestamp;
        record
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
        self.flush()?;
        let mut paths = log_paths(&self.inner.root)?;
        paths.sort();
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let file = File::open(&path)?;
            let length = file.metadata()?.len();
            if length > 0 {
                files.push(SnapshotFile { file, length });
            }
        }
        let total_bytes = files.iter().map(|file| file.length).sum();
        let coverage_start_utc = files.first().and_then(first_timestamp);
        let coverage_end_utc = files.last().and_then(last_timestamp);
        let metadata = LogSnapshotMetadata {
            snapshot_id: Uuid::new_v4(),
            total_bytes,
            file_count: files.len(),
            coverage_start_utc,
            coverage_end_utc,
            retention_truncated: self.inner.root.join("retention-truncated").exists(),
            dropped_records: self.inner.dropped_records.load(Ordering::Relaxed),
            writer_errors: self.inner.writer_errors.load(Ordering::Relaxed),
        };
        Ok(LogSnapshot {
            metadata,
            files,
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
    hasher: Sha256,
    next_offset: u64,
    last_chunk: Option<LogExportChunk>,
    completed_hash: Option<String>,
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

fn first_timestamp(snapshot_file: &SnapshotFile) -> Option<String> {
    let mut file = snapshot_file.file.try_clone().ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut line = Vec::new();
    BufReader::new(file).read_until(b'\n', &mut line).ok()?;
    serde_json::from_slice::<DiagnosticLogRecord>(&line)
        .ok()
        .map(|record| record.timestamp_utc)
}

fn last_timestamp(snapshot_file: &SnapshotFile) -> Option<String> {
    let mut file = snapshot_file.file.try_clone().ok()?;
    let tail_length = snapshot_file.length.min(MAX_LOG_RECORD_BYTES as u64) as usize;
    file.seek(SeekFrom::End(-(tail_length as i64))).ok()?;
    let mut tail = Vec::with_capacity(tail_length);
    file.read_to_end(&mut tail).ok()?;
    let line = tail.split(|byte| *byte == b'\n').rev().find(|line| !line.is_empty())?;
    serde_json::from_slice::<DiagnosticLogRecord>(line)
        .ok()
        .map(|record| record.timestamp_utc)
}

fn bound_record(record: &mut DiagnosticLogRecord) {
    record.level = record.level.chars().take(16).collect();
    record.target = record.target.chars().take(256).collect();
    record.message = record.message.chars().take(2_000).collect();
    record.source_timestamp = record
        .source_timestamp
        .take()
        .map(|value| value.chars().take(256).collect());
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
        assert!(DateTime::parse_from_rfc3339(&saved.timestamp_utc).is_ok());
        assert!(DateTime::parse_from_rfc3339(&saved.timestamp_local).is_ok());
        assert!(saved.monotonic_ms <= monotonic_ms());
        drop(store);
        fs::remove_dir_all(dir).unwrap();
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
