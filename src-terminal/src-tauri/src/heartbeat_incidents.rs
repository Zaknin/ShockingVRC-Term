use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::diagnostics::{DiagnosticEvent, DiagnosticKind};

pub const MAX_HEARTBEAT_INCIDENTS: usize = 20;
const MAX_CAPTURE_EVENTS_PER_PHASE: usize = 300;
const MAX_INCIDENT_FILE_BYTES: u64 = 8 * 1024 * 1024;
const INCIDENT_WRITER_QUEUE_CAPACITY: usize = 8;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatIncidentQueueMetrics {
    pub action_queue_depth: usize,
    pub action_queue_capacity: usize,
    pub pending_heartbeat_requests: usize,
    pub data_channel_sender_available: Option<bool>,
    pub ice_pair_available: bool,
    pub ice_servers_configured: Option<bool>,
    pub rendezvous_stun_received: Option<bool>,
    pub turn_credentials_received: Option<bool>,
    pub local_candidates_gathered: Option<bool>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatIncidentOutcome {
    InProgress,
    Recovered,
    SessionEnded,
    ProcessRestartedDuringRecovery,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatIncident {
    pub id: Uuid,
    pub started_utc: String,
    pub completed_utc: Option<String>,
    pub outcome: HeartbeatIncidentOutcome,
    pub room_id: Option<Uuid>,
    pub generation: Option<u64>,
    pub transport: Option<String>,
    pub candidate_pair: Option<String>,
    pub queue_metrics: HeartbeatIncidentQueueMetrics,
    pub prelude_events: Vec<DiagnosticEvent>,
    pub recovery_events: Vec<DiagnosticEvent>,
    pub recovery_events_dropped: u64,
}

#[derive(Default)]
struct IncidentState {
    records: VecDeque<HeartbeatIncident>,
    active_id: Option<Uuid>,
}

enum WriterMessage {
    Persist,
    Flush(mpsc::SyncSender<Result<(), String>>),
}

#[derive(Clone)]
pub struct HeartbeatIncidentStore {
    state: Arc<Mutex<IncidentState>>,
    writer: Option<SyncSender<WriterMessage>>,
    persistence_errors: Arc<AtomicU64>,
}

impl Default for HeartbeatIncidentStore {
    fn default() -> Self {
        Self::memory()
    }
}

impl HeartbeatIncidentStore {
    pub fn memory() -> Self {
        Self {
            state: Arc::new(Mutex::new(IncidentState::default())),
            writer: None,
            persistence_errors: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut records = read_incidents(&path)?;
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut recovered_in_progress = false;
        for incident in &mut records {
            if incident.outcome == HeartbeatIncidentOutcome::InProgress {
                incident.outcome = HeartbeatIncidentOutcome::ProcessRestartedDuringRecovery;
                incident.completed_utc = Some(now.clone());
                recovered_in_progress = true;
            }
        }
        trim_incidents(&mut records);
        let state = Arc::new(Mutex::new(IncidentState {
            records,
            active_id: None,
        }));
        let (writer, receiver) = mpsc::sync_channel(INCIDENT_WRITER_QUEUE_CAPACITY);
        let state_for_writer = Arc::clone(&state);
        let errors = Arc::new(AtomicU64::new(0));
        let errors_for_writer = Arc::clone(&errors);
        thread::Builder::new()
            .name("shockingvrc-heartbeat-incident-writer".to_owned())
            .spawn(move || incident_writer_loop(path, state_for_writer, receiver, errors_for_writer))?;
        let store = Self {
            state,
            writer: Some(writer),
            persistence_errors: errors,
        };
        if recovered_in_progress {
            store.schedule_persist();
        }
        Ok(store)
    }

    pub fn snapshot(&self) -> Vec<HeartbeatIncident> {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.records.iter().cloned().collect()
    }

    pub fn persistence_errors(&self) -> u64 {
        self.persistence_errors.load(Ordering::Relaxed)
    }

    pub fn flush(&self) -> io::Result<()> {
        let Some(writer) = &self.writer else {
            return Ok(());
        };
        let (reply, response) = mpsc::sync_channel(1);
        writer
            .send(WriterMessage::Flush(reply))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "heartbeat incident writer stopped"))?;
        response
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "heartbeat incident writer stopped"))?
            .map_err(io::Error::other)
    }

    pub fn record_event(
        &self,
        event: &DiagnosticEvent,
        prelude: Option<Vec<DiagnosticEvent>>,
        queue_metrics: Option<HeartbeatIncidentQueueMetrics>,
    ) {
        let mut persist = false;
        {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if event.kind == DiagnosticKind::HeartbeatTimeout && state.active_id.is_none() {
                let prelude_events = prelude.unwrap_or_else(|| vec![event.clone()]);
                let incident = HeartbeatIncident {
                    id: Uuid::new_v4(),
                    started_utc: event.timestamp_utc.clone(),
                    completed_utc: None,
                    outcome: HeartbeatIncidentOutcome::InProgress,
                    room_id: event.room_id,
                    generation: event.generation,
                    transport: event.transport.clone(),
                    candidate_pair: event.candidate_pair.clone(),
                    queue_metrics: queue_metrics.unwrap_or_default(),
                    prelude_events: bounded_events(prelude_events),
                    recovery_events: Vec::new(),
                    recovery_events_dropped: 0,
                };
                state.active_id = Some(incident.id);
                state.records.push_back(incident);
                trim_incidents(&mut state.records);
                persist = true;
            } else if let Some(active_id) = state.active_id {
                if let Some(incident) = state.records.iter_mut().find(|record| record.id == active_id) {
                    append_bounded(
                        &mut incident.recovery_events,
                        event.clone(),
                        &mut incident.recovery_events_dropped,
                    );
                    let resumed = incident
                        .recovery_events
                        .iter()
                        .any(|candidate| candidate.kind == DiagnosticKind::ResumeSucceeded);
                    let outcome = if event.kind == DiagnosticKind::SessionEnded {
                        Some(HeartbeatIncidentOutcome::SessionEnded)
                    } else if resumed
                        && ((event.kind == DiagnosticKind::ConnectionState && event.phase.as_deref() == Some("online"))
                            || event.kind == DiagnosticKind::HeartbeatAck)
                    {
                        Some(HeartbeatIncidentOutcome::Recovered)
                    } else {
                        None
                    };
                    if let Some(outcome) = outcome {
                        incident.outcome = outcome;
                        incident.completed_utc = Some(event.timestamp_utc.clone());
                        state.active_id = None;
                        trim_incidents(&mut state.records);
                        persist = true;
                    } else if matches!(
                        event.kind,
                        DiagnosticKind::ConnectionState
                            | DiagnosticKind::ResumeStarted
                            | DiagnosticKind::ResumeSucceeded
                            | DiagnosticKind::ResumeFailed
                            | DiagnosticKind::SignalingLeaseLost
                    ) {
                        persist = true;
                    }
                } else {
                    state.active_id = None;
                }
            }
        }
        if persist {
            self.schedule_persist();
        }
    }

    fn schedule_persist(&self) {
        let Some(writer) = &self.writer else {
            return;
        };
        match writer.try_send(WriterMessage::Persist) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                self.persistence_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn bounded_events(mut events: Vec<DiagnosticEvent>) -> Vec<DiagnosticEvent> {
    if events.len() > MAX_CAPTURE_EVENTS_PER_PHASE {
        events.drain(..events.len() - MAX_CAPTURE_EVENTS_PER_PHASE);
    }
    events
}

fn append_bounded(events: &mut Vec<DiagnosticEvent>, event: DiagnosticEvent, dropped: &mut u64) {
    if events.len() == MAX_CAPTURE_EVENTS_PER_PHASE {
        events.remove(0);
        *dropped = dropped.saturating_add(1);
    }
    events.push(event);
}

fn trim_incidents(records: &mut VecDeque<HeartbeatIncident>) {
    while records.len() > MAX_HEARTBEAT_INCIDENTS {
        records.pop_front();
    }
}

fn read_incidents(path: &Path) -> io::Result<VecDeque<HeartbeatIncident>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(VecDeque::new()),
        Err(error) => return Err(error),
    };
    let length = file.metadata()?.len();
    if length > MAX_INCIDENT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "heartbeat incident history exceeds its size limit",
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.read_to_end(&mut bytes)?;
    let mut records: VecDeque<HeartbeatIncident> =
        serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    trim_incidents(&mut records);
    Ok(records)
}

fn incident_writer_loop(
    path: PathBuf,
    state: Arc<Mutex<IncidentState>>,
    receiver: mpsc::Receiver<WriterMessage>,
    persistence_errors: Arc<AtomicU64>,
) {
    while let Ok(message) = receiver.recv() {
        match message {
            WriterMessage::Persist => {
                if write_incident_snapshot(&path, &state).is_err() {
                    persistence_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            WriterMessage::Flush(reply) => {
                let result = write_incident_snapshot(&path, &state).map_err(|error| error.to_string());
                if result.is_err() {
                    persistence_errors.fetch_add(1, Ordering::Relaxed);
                }
                let _ = reply.send(result);
            }
        }
    }
}

fn write_incident_snapshot(path: &Path, state: &Mutex<IncidentState>) -> io::Result<()> {
    let records = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .records
        .clone();
    let bytes = serde_json::to_vec(&records).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_INCIDENT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "heartbeat incident history exceeds its size limit",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "incident path has no parent"))?;
    let temporary = parent.join(format!("heartbeat-incidents-{}.part", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> PathBuf {
        std::env::temp_dir().join(format!("shockingvrc-heartbeat-incidents-{}.json", Uuid::new_v4()))
    }

    fn event(kind: DiagnosticKind, monotonic_ms: u64) -> DiagnosticEvent {
        DiagnosticEvent {
            timestamp_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            process_instance_id: Some(Uuid::new_v4()),
            monotonic_ms,
            kind,
            phase: None,
            room_id: Some(Uuid::new_v4()),
            request_id: None,
            generation: Some(3),
            transport: Some("webrtc_datachannel".to_owned()),
            candidate_pair: Some("RELAY/UDP↔RELAY/UDP".to_owned()),
            payload_bytes: None,
            heartbeat_ack_latency_ms: None,
        }
    }

    #[test]
    fn incident_is_persisted_and_reloaded_after_runtime_ring_would_overwrite_it() {
        let path = test_path();
        let store = HeartbeatIncidentStore::open(&path).unwrap();
        let prelude = vec![
            event(DiagnosticKind::HeartbeatTx, 100),
            event(DiagnosticKind::HeartbeatAck, 120),
            event(DiagnosticKind::HeartbeatTimeout, 10_000),
        ];
        let timeout = prelude.last().unwrap().clone();
        store.record_event(
            &timeout,
            Some(prelude.clone()),
            Some(HeartbeatIncidentQueueMetrics {
                action_queue_depth: 2,
                action_queue_capacity: 8,
                pending_heartbeat_requests: 3,
                data_channel_sender_available: Some(true),
                ice_pair_available: true,
                ..HeartbeatIncidentQueueMetrics::default()
            }),
        );
        store.record_event(&event(DiagnosticKind::ResumeStarted, 10_100), None, None);
        store.record_event(&event(DiagnosticKind::ResumeSucceeded, 10_500), None, None);
        let mut online = event(DiagnosticKind::ConnectionState, 10_600);
        online.phase = Some("online".to_owned());
        store.record_event(&online, None, None);
        store.flush().unwrap();

        for index in 0..3500 {
            store.record_event(&event(DiagnosticKind::ApiFrame, 11_000 + index), None, None);
        }
        let recovered = store.snapshot();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].outcome, HeartbeatIncidentOutcome::Recovered);
        assert_eq!(recovered[0].prelude_events.len(), 3);
        assert!(
            recovered[0]
                .recovery_events
                .iter()
                .any(|item| item.kind == DiagnosticKind::ResumeSucceeded)
        );
        assert_eq!(recovered[0].queue_metrics.action_queue_depth, 2);
        store.flush().unwrap();
        drop(store);

        let reopened = HeartbeatIncidentStore::open(&path).unwrap();
        let persisted = reopened.snapshot();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].outcome, HeartbeatIncidentOutcome::Recovered);
        let encoded = serde_json::to_string(&persisted).unwrap();
        assert!(encoded.contains("heartbeat_timeout"));
        assert!(!encoded.contains("access_token"));
        assert!(!encoded.contains("192.168."));
        reopened.flush().unwrap();
        drop(reopened);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn in_progress_recovery_milestones_are_persisted_before_process_restart() {
        let path = test_path();
        let store = HeartbeatIncidentStore::open(&path).unwrap();
        let timeout = event(DiagnosticKind::HeartbeatTimeout, 100);
        store.record_event(&timeout, Some(vec![timeout.clone()]), None);
        store.flush().unwrap();

        let resume_started = event(DiagnosticKind::ResumeStarted, 200);
        store.record_event(&resume_started, None, None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let persisted = read_incidents(&path).unwrap();
            if persisted[0]
                .recovery_events
                .iter()
                .any(|item| item.kind == DiagnosticKind::ResumeStarted)
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "recovery milestone was not persisted");
            thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(store);

        let reopened = HeartbeatIncidentStore::open(&path).unwrap();
        let incident = reopened.snapshot().remove(0);
        assert_eq!(incident.outcome, HeartbeatIncidentOutcome::ProcessRestartedDuringRecovery);
        assert!(
            incident
                .recovery_events
                .iter()
                .any(|item| item.kind == DiagnosticKind::ResumeStarted)
        );
        reopened.flush().unwrap();
        drop(reopened);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn retained_incidents_and_each_event_phase_are_bounded() {
        let store = HeartbeatIncidentStore::memory();
        for incident_index in 0..MAX_HEARTBEAT_INCIDENTS + 5 {
            let timeout = event(DiagnosticKind::HeartbeatTimeout, incident_index as u64);
            store.record_event(&timeout, Some(vec![timeout.clone()]), None);
            for event_index in 0..(MAX_CAPTURE_EVENTS_PER_PHASE + 3) {
                store.record_event(&event(DiagnosticKind::ApiFrame, event_index as u64), None, None);
            }
            let mut online = event(DiagnosticKind::ConnectionState, 1_000);
            online.phase = Some("online".to_owned());
            store.record_event(&event(DiagnosticKind::ResumeSucceeded, 999), None, None);
            store.record_event(&online, None, None);
        }
        let incidents = store.snapshot();
        assert_eq!(incidents.len(), MAX_HEARTBEAT_INCIDENTS);
        assert!(
            incidents
                .iter()
                .all(|incident| incident.prelude_events.len() <= MAX_CAPTURE_EVENTS_PER_PHASE)
        );
        assert!(
            incidents
                .iter()
                .all(|incident| incident.recovery_events.len() <= MAX_CAPTURE_EVENTS_PER_PHASE)
        );
        assert!(incidents.iter().all(|incident| incident.recovery_events_dropped > 0));
    }

    #[test]
    fn oversized_incident_snapshot_is_not_written_to_disk() {
        let root = std::env::temp_dir().join(format!("shockingvrc-heartbeat-cap-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("heartbeat-incidents.json");
        let store = HeartbeatIncidentStore::open(&path).unwrap();
        let timeout = event(DiagnosticKind::HeartbeatTimeout, 100);
        let mut oversized = timeout.clone();
        oversized.phase = Some("x".repeat(MAX_INCIDENT_FILE_BYTES as usize));
        store.record_event(&timeout, Some(vec![oversized]), None);

        assert!(store.flush().is_err());
        assert!(store.persistence_errors() > 0);
        if let Ok(metadata) = fs::metadata(&path) {
            assert!(metadata.len() <= MAX_INCIDENT_FILE_BYTES);
        }
        assert!(
            fs::read_dir(&root)
                .unwrap()
                .all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".part"))
        );
        drop(store);
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(root);
    }
}
