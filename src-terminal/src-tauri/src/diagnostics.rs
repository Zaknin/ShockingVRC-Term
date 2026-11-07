use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use serde::Serialize;
use uuid::Uuid;

pub const MAX_DIAGNOSTIC_EVENTS: usize = 300;
pub const MAX_TRANSPORT_LOG_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    ConnectionAttempt,
    ConnectionEstablished,
    ConnectionFailed,
    ConnectionState,
    SessionStarted,
    SessionEnded,
    HeartbeatTx,
    HeartbeatAck,
    HeartbeatTimeout,
    SnapshotRequest,
    SnapshotResponse,
    ResumeStarted,
    ResumeSucceeded,
    ResumeFailed,
    SignalingLeaseLost,
    DisconnectRequested,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DiagnosticEvent {
    pub monotonic_ms: u64,
    pub kind: DiagnosticKind,
    pub phase: Option<String>,
    pub room_id: Option<Uuid>,
    pub request_id: Option<Uuid>,
    pub generation: Option<u64>,
    pub transport: Option<String>,
    pub candidate_pair: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DiagnosticCounters {
    pub connection_attempts: u64,
    pub connection_established: u64,
    pub connection_failed: u64,
    pub heartbeat_tx: u64,
    pub heartbeat_ack: u64,
    pub heartbeat_timeouts: u64,
    pub snapshot_requests: u64,
    pub snapshot_responses: u64,
    pub resume_attempts: u64,
    pub resume_succeeded: u64,
    pub resume_failed: u64,
    pub signaling_lease_lost: u64,
    pub disconnect_requested: u64,
    pub generation_changes: u64,
}

#[derive(Clone, Debug, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DiagnosticSnapshot {
    pub counters: DiagnosticCounters,
    pub events: Vec<DiagnosticEvent>,
    pub current_room_id: Option<Uuid>,
    pub current_generation: Option<u64>,
    pub last_heartbeat_age_ms: Option<u64>,
}

#[derive(Default)]
struct RecorderState {
    counters: DiagnosticCounters,
    events: VecDeque<DiagnosticEvent>,
    current_room_id: Option<Uuid>,
    current_generation: Option<u64>,
    last_heartbeat_ms: Option<u64>,
    last_phase: Option<&'static str>,
}

pub struct DiagnosticRecorder {
    started: Instant,
    state: std::sync::Mutex<RecorderState>,
}

impl Default for DiagnosticRecorder {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            state: std::sync::Mutex::new(RecorderState::default()),
        }
    }
}

impl DiagnosticRecorder {
    pub fn record(
        &self,
        kind: DiagnosticKind,
        room_id: Option<Uuid>,
        request_id: Option<Uuid>,
        generation: Option<u64>,
        transport: Option<&str>,
        candidate_pair: Option<&str>,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        match kind {
            DiagnosticKind::ConnectionAttempt => state.counters.connection_attempts += 1,
            DiagnosticKind::ConnectionEstablished => state.counters.connection_established += 1,
            DiagnosticKind::ConnectionFailed => state.counters.connection_failed += 1,
            DiagnosticKind::HeartbeatTx => state.counters.heartbeat_tx += 1,
            DiagnosticKind::HeartbeatAck => {
                state.counters.heartbeat_ack += 1;
                state.last_heartbeat_ms = Some(now_ms);
            }
            DiagnosticKind::HeartbeatTimeout => state.counters.heartbeat_timeouts += 1,
            DiagnosticKind::SnapshotRequest => state.counters.snapshot_requests += 1,
            DiagnosticKind::SnapshotResponse => state.counters.snapshot_responses += 1,
            DiagnosticKind::ResumeStarted => state.counters.resume_attempts += 1,
            DiagnosticKind::ResumeSucceeded => state.counters.resume_succeeded += 1,
            DiagnosticKind::ResumeFailed => state.counters.resume_failed += 1,
            DiagnosticKind::SignalingLeaseLost => state.counters.signaling_lease_lost += 1,
            DiagnosticKind::DisconnectRequested => state.counters.disconnect_requested += 1,
            DiagnosticKind::SessionStarted | DiagnosticKind::ConnectionState | DiagnosticKind::SessionEnded => {}
        }

        if matches!(kind, DiagnosticKind::SessionStarted | DiagnosticKind::ResumeSucceeded) {
            if let Some(new_generation) = generation {
                if state
                    .current_generation
                    .is_some_and(|current| current != new_generation)
                {
                    state.counters.generation_changes += 1;
                }
                state.current_generation = Some(new_generation);
            }
            if room_id.is_some() {
                state.current_room_id = room_id;
            }
        } else if kind == DiagnosticKind::SessionEnded {
            state.current_room_id = None;
            state.current_generation = None;
        }

        push_event(
            &mut state,
            DiagnosticEvent {
                monotonic_ms: now_ms,
                kind,
                phase: None,
                room_id,
                request_id,
                generation,
                transport: safe_transport(transport).map(str::to_owned),
                candidate_pair: candidate_pair.and_then(candidate_pair_types),
            },
        );
    }

    pub fn record_connection_phase(&self, phase: &str) {
        let safe_phase = match phase {
            "connecting" => "connecting",
            "identity_verified" => "identity_verified",
            "online" => "online",
            "reconnecting" => "reconnecting",
            "user_disconnected" => "user_disconnected",
            "host_unavailable" => "host_unavailable",
            "transport_lost" => "transport_lost",
            "session_revoked" => "session_revoked",
            _ => "other",
        };
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.last_phase == Some(safe_phase) {
            return;
        }
        state.last_phase = Some(safe_phase);
        let now_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let room_id = state.current_room_id;
        let generation = state.current_generation;
        push_event(
            &mut state,
            DiagnosticEvent {
                monotonic_ms: now_ms,
                kind: DiagnosticKind::ConnectionState,
                phase: Some(safe_phase.to_owned()),
                room_id,
                request_id: None,
                generation,
                transport: None,
                candidate_pair: None,
            },
        );
    }

    pub fn snapshot(&self) -> DiagnosticSnapshot {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        DiagnosticSnapshot {
            counters: state.counters.clone(),
            events: state.events.iter().cloned().collect(),
            current_room_id: state.current_room_id,
            current_generation: state.current_generation,
            last_heartbeat_age_ms: state.last_heartbeat_ms.map(|last| now_ms.saturating_sub(last)),
        }
    }

    pub fn clear_events_and_counters(&self) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.counters = DiagnosticCounters::default();
        state.events.clear();
    }
}

fn push_event(state: &mut RecorderState, event: DiagnosticEvent) {
    if state.events.len() == MAX_DIAGNOSTIC_EVENTS {
        state.events.pop_front();
    }
    state.events.push_back(event);
}

fn safe_transport(value: Option<&str>) -> Option<&'static str> {
    match value {
        Some("direct_wss") => Some("direct_wss"),
        Some("webrtc_datachannel") => Some("webrtc_datachannel"),
        _ => None,
    }
}

fn candidate_pair_types(value: &str) -> Option<String> {
    let mut candidate_types = Vec::new();
    let mut transport = None;
    for token in value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        let token = token.to_ascii_uppercase();
        if candidate_types.len() < 2 && matches!(token.as_str(), "HOST" | "SRFLX" | "PRFLX" | "RELAY") {
            candidate_types.push(token);
        } else if transport.is_none() && matches!(token.as_str(), "UDP" | "TCP") {
            transport = Some(token);
        }
    }
    if candidate_types.len() != 2 {
        return None;
    }
    let transport = transport?;
    Some(format!(
        "{}/{}↔{}/{}",
        candidate_types[0], transport, candidate_types[1], transport
    ))
}

pub fn previous_transport_log_path(path: &Path) -> PathBuf {
    path.with_file_name("transport.previous.log")
}

pub struct RotatingLogWriter {
    active_path: PathBuf,
    previous_path: PathBuf,
    file: File,
    length: u64,
}

impl RotatingLogWriter {
    pub fn open(active_path: &Path) -> io::Result<Self> {
        if let Some(parent) = active_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let previous_path = previous_transport_log_path(active_path);
        trim_file_tail(active_path, MAX_TRANSPORT_LOG_BYTES)?;
        trim_file_tail(&previous_path, MAX_TRANSPORT_LOG_BYTES)?;
        let file = OpenOptions::new().create(true).append(true).open(active_path)?;
        let length = file.metadata()?.len();
        Ok(Self {
            active_path: active_path.to_owned(),
            previous_path,
            file,
            length,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let temp_path = self.active_path.with_extension("rotate.tmp");
        let replacement = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)?;
        let old_file = std::mem::replace(&mut self.file, replacement);
        drop(old_file);
        if self.previous_path.exists() {
            fs::remove_file(&self.previous_path)?;
        }
        if self.active_path.exists() {
            fs::rename(&self.active_path, &self.previous_path)?;
        }
        fs::rename(&temp_path, &self.active_path)?;
        self.file = OpenOptions::new().create(true).append(true).open(&self.active_path)?;
        self.length = 0;
        Ok(())
    }
}

impl Write for RotatingLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.length = self.file.metadata()?.len();
        if self.length >= MAX_TRANSPORT_LOG_BYTES
            || self.length.saturating_add(bytes.len() as u64) > MAX_TRANSPORT_LOG_BYTES
        {
            self.rotate()?;
        }
        let available = MAX_TRANSPORT_LOG_BYTES.saturating_sub(self.length) as usize;
        let count = bytes.len().min(available);
        if count == 0 && !bytes.is_empty() {
            self.rotate()?;
            return self.write(bytes);
        }
        let written = self.file.write(&bytes[..count])?;
        self.length = self.length.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn trim_file_tail(path: &Path, maximum: u64) -> io::Result<()> {
    let mut input = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let length = input.metadata()?.len();
    if length <= maximum {
        return Ok(());
    }
    let temp_path = path.with_extension("trim.tmp");
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp_path)?;
    input.seek(SeekFrom::Start(length - maximum))?;
    io::copy(&mut input, &mut output)?;
    output.flush()?;
    drop(output);
    drop(input);
    fs::remove_file(path)?;
    fs::rename(temp_path, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shockingvrc-diagnostics-{}-{name}.log", Uuid::new_v4()))
    }

    #[test]
    fn recorder_exports_bounded_safe_lifecycle_events_and_counters() {
        let recorder = DiagnosticRecorder::default();
        let room_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        recorder.record(
            DiagnosticKind::SessionStarted,
            Some(room_id),
            None,
            Some(1),
            Some("webrtc_datachannel"),
            Some("RELAY/UDP 192.168.1.2 ↔ RELAY/UDP 8.8.8.8"),
        );
        recorder.record(
            DiagnosticKind::HeartbeatTx,
            Some(room_id),
            Some(request_id),
            Some(1),
            Some("webrtc_datachannel"),
            None,
        );
        recorder.record(
            DiagnosticKind::HeartbeatAck,
            Some(room_id),
            Some(request_id),
            Some(1),
            Some("webrtc_datachannel"),
            None,
        );
        recorder.record(
            DiagnosticKind::SnapshotRequest,
            Some(room_id),
            Some(request_id),
            Some(1),
            Some("webrtc_datachannel"),
            None,
        );
        recorder.record(
            DiagnosticKind::SnapshotResponse,
            Some(room_id),
            Some(request_id),
            Some(1),
            Some("webrtc_datachannel"),
            None,
        );
        let value = serde_json::to_value(recorder.snapshot()).unwrap();
        assert_eq!(value["counters"]["heartbeat_tx"], 1);
        assert_eq!(value["counters"]["heartbeat_ack"], 1);
        assert_eq!(value["counters"]["snapshot_requests"], 1);
        assert_eq!(value["counters"]["snapshot_responses"], 1);
        assert_eq!(value["current_room_id"], room_id.to_string());
        let serialized = value.to_string();
        assert!(!serialized.contains("192.168.1.2"));
        assert!(!serialized.contains("8.8.8.8"));
        assert!(serialized.contains("RELAY/UDP↔RELAY/UDP"));
    }

    #[test]
    fn event_ring_is_bounded() {
        let recorder = DiagnosticRecorder::default();
        for _ in 0..(MAX_DIAGNOSTIC_EVENTS + 7) {
            recorder.record(DiagnosticKind::HeartbeatTx, None, None, None, None, None);
        }
        assert_eq!(recorder.snapshot().events.len(), MAX_DIAGNOSTIC_EVENTS);
    }

    #[test]
    fn rotating_writer_keeps_two_bounded_generations_and_trims_legacy_logs() {
        let active = temp_path("active");
        let previous = previous_transport_log_path(&active);
        fs::write(&active, vec![b'x'; (MAX_TRANSPORT_LOG_BYTES + 10) as usize]).unwrap();
        let mut writer = RotatingLogWriter::open(&active).unwrap();
        assert_eq!(fs::metadata(&active).unwrap().len(), MAX_TRANSPORT_LOG_BYTES);
        writer
            .write_all(&vec![b'y'; (MAX_TRANSPORT_LOG_BYTES + 10) as usize])
            .unwrap();
        writer.flush().unwrap();
        assert!(fs::metadata(&active).unwrap().len() <= MAX_TRANSPORT_LOG_BYTES);
        assert!(fs::metadata(&previous).unwrap().len() <= MAX_TRANSPORT_LOG_BYTES);
        assert!(
            fs::metadata(&active).unwrap().len() + fs::metadata(&previous).unwrap().len()
                <= 2 * MAX_TRANSPORT_LOG_BYTES
        );
        let _ = fs::remove_file(active);
        let _ = fs::remove_file(previous);
    }
}
