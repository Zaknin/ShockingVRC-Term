use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    time::Instant,
};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::heartbeat_incidents::{HeartbeatIncidentQueueMetrics, HeartbeatIncidentStore};

pub const MAX_DIAGNOSTIC_EVENTS: usize = 300;

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
    FullSnapshotFrame,
    TelemetryFrame,
    ApiFrame,
    TelemetryNegotiated,
    ResumeStarted,
    ResumeSucceeded,
    ResumeFailed,
    SignalingLeaseLost,
    DisconnectRequested,
}

#[derive(Clone, Copy, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiTrafficDirection {
    Tx,
    Rx,
}

#[derive(Clone, Copy, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiTrafficCategory {
    Heartbeat,
    State,
    Command,
    Authentication,
    Session,
    Other,
}

#[derive(Clone, Debug, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ApiTrafficDirectionCounters {
    pub messages: u64,
    pub payload_bytes: u64,
    /// Average messages per second since the API counter window began, scaled by 1,000.
    pub average_messages_per_sec_milli_since_window_start: u64,
    /// Average payload bytes per second since the API counter window began, scaled by 1,000.
    pub average_payload_bytes_per_sec_milli_since_window_start: u64,
}

#[derive(Clone, Debug, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ApiTrafficCategoryCounters {
    pub tx: ApiTrafficDirectionCounters,
    pub rx: ApiTrafficDirectionCounters,
}

#[derive(Clone, Debug, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ApiTrafficCounters {
    pub heartbeat: ApiTrafficCategoryCounters,
    pub state: ApiTrafficCategoryCounters,
    pub command: ApiTrafficCategoryCounters,
    pub authentication: ApiTrafficCategoryCounters,
    pub session: ApiTrafficCategoryCounters,
    pub other: ApiTrafficCategoryCounters,
}

impl ApiTrafficCounters {
    fn category_mut(&mut self, category: ApiTrafficCategory) -> &mut ApiTrafficCategoryCounters {
        match category {
            ApiTrafficCategory::Heartbeat => &mut self.heartbeat,
            ApiTrafficCategory::State => &mut self.state,
            ApiTrafficCategory::Command => &mut self.command,
            ApiTrafficCategory::Authentication => &mut self.authentication,
            ApiTrafficCategory::Session => &mut self.session,
            ApiTrafficCategory::Other => &mut self.other,
        }
    }
}

impl ApiTrafficDirection {
    fn label(self) -> &'static str {
        match self {
            Self::Tx => "tx",
            Self::Rx => "rx",
        }
    }
}

impl ApiTrafficCategory {
    fn label(self) -> &'static str {
        match self {
            Self::Heartbeat => "heartbeat",
            Self::State => "state",
            Self::Command => "command",
            Self::Authentication => "authentication",
            Self::Session => "session",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DiagnosticEvent {
    #[serde(default)]
    pub timestamp_utc: String,
    #[serde(default)]
    pub process_instance_id: Option<Uuid>,
    pub monotonic_ms: u64,
    pub kind: DiagnosticKind,
    pub phase: Option<String>,
    pub room_id: Option<Uuid>,
    pub request_id: Option<Uuid>,
    pub generation: Option<u64>,
    pub transport: Option<String>,
    pub candidate_pair: Option<String>,
    #[serde(default)]
    pub payload_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_ack_latency_ms: Option<u64>,
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
    pub full_snapshot_frames: u64,
    pub telemetry_frames: u64,
    pub telemetry_negotiations: u64,
    pub full_snapshot_payload_bytes: u64,
    pub telemetry_payload_bytes: u64,
    pub telemetry_render_samples: u64,
    pub telemetry_render_total_ms: u64,
    pub telemetry_render_max_ms: u64,
    pub resume_attempts: u64,
    pub resume_succeeded: u64,
    pub resume_failed: u64,
    pub signaling_lease_lost: u64,
    pub disconnect_requested: u64,
    pub generation_changes: u64,
    #[serde(default)]
    pub api_traffic: ApiTrafficCounters,
}

#[derive(Clone, Debug, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DiagnosticSnapshot {
    pub counters: DiagnosticCounters,
    pub events: Vec<DiagnosticEvent>,
    pub current_room_id: Option<Uuid>,
    pub current_generation: Option<u64>,
    pub last_heartbeat_age_ms: Option<u64>,
    pub contact_update_age_ms: Option<u64>,
    #[serde(default)]
    pub observed_ms: u64,
    #[serde(default)]
    pub api_traffic_observed_ms: u64,
    #[serde(default)]
    pub process_instance_id: Option<Uuid>,
    #[serde(default)]
    pub monotonic_clock_origin_utc: Option<String>,
    #[serde(default)]
    pub runtime_coverage_start_utc: Option<String>,
    #[serde(default)]
    pub runtime_coverage_end_utc: Option<String>,
    #[serde(default)]
    pub retained_event_count: u64,
    #[serde(default)]
    pub exported_event_count: u64,
    #[serde(default)]
    pub events_overwritten: u64,
    #[serde(default)]
    pub events_dropped: u64,
    #[serde(default)]
    pub events_excluded_by_time_range: u64,
    #[serde(default)]
    pub events_unknown_timestamp: u64,
    #[serde(default)]
    pub counters_scope: Option<String>,
}

#[derive(Default)]
struct RecorderState {
    counters: DiagnosticCounters,
    events: VecDeque<DiagnosticEvent>,
    current_room_id: Option<Uuid>,
    current_generation: Option<u64>,
    last_heartbeat_ms: Option<u64>,
    last_contact_update_ms: Option<u64>,
    last_phase: Option<&'static str>,
    api_traffic_started_ms: Option<u64>,
    events_overwritten: u64,
}

pub struct DiagnosticRecorder {
    started: Instant,
    started_utc: String,
    process_instance_id: Uuid,
    state: std::sync::Mutex<RecorderState>,
    heartbeat_incidents: HeartbeatIncidentStore,
}

impl Default for DiagnosticRecorder {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            started_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            process_instance_id: Uuid::new_v4(),
            state: std::sync::Mutex::new(RecorderState::default()),
            heartbeat_incidents: HeartbeatIncidentStore::memory(),
        }
    }
}

impl DiagnosticRecorder {
    pub fn open(incident_path: impl Into<PathBuf>) -> io::Result<Self> {
        Ok(Self::with_heartbeat_incidents(HeartbeatIncidentStore::open(incident_path)?))
    }

    fn with_heartbeat_incidents(heartbeat_incidents: HeartbeatIncidentStore) -> Self {
        Self {
            started: Instant::now(),
            started_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            process_instance_id: Uuid::new_v4(),
            state: std::sync::Mutex::new(RecorderState::default()),
            heartbeat_incidents,
        }
    }

    pub fn heartbeat_incidents(&self) -> HeartbeatIncidentStore {
        self.heartbeat_incidents.clone()
    }

    pub fn record(
        &self,
        kind: DiagnosticKind,
        room_id: Option<Uuid>,
        request_id: Option<Uuid>,
        generation: Option<u64>,
        transport: Option<&str>,
        candidate_pair: Option<&str>,
    ) {
        self.record_with_context(kind, room_id, request_id, generation, transport, candidate_pair, None, None);
    }

    pub fn record_heartbeat_timeout(
        &self,
        room_id: Option<Uuid>,
        generation: Option<u64>,
        transport: Option<&str>,
        candidate_pair: Option<&str>,
        queue_metrics: HeartbeatIncidentQueueMetrics,
    ) {
        self.record_with_context(
            DiagnosticKind::HeartbeatTimeout,
            room_id,
            None,
            generation,
            transport,
            candidate_pair,
            None,
            Some(queue_metrics),
        );
    }

    pub fn record_heartbeat_ack(
        &self,
        room_id: Option<Uuid>,
        request_id: Option<Uuid>,
        generation: Option<u64>,
        transport: Option<&str>,
        candidate_pair: Option<&str>,
        ack_latency_ms: u64,
    ) {
        self.record_with_context(
            DiagnosticKind::HeartbeatAck,
            room_id,
            request_id,
            generation,
            transport,
            candidate_pair,
            Some(ack_latency_ms),
            None,
        );
    }

    fn record_with_context(
        &self,
        kind: DiagnosticKind,
        room_id: Option<Uuid>,
        request_id: Option<Uuid>,
        generation: Option<u64>,
        transport: Option<&str>,
        candidate_pair: Option<&str>,
        heartbeat_ack_latency_ms: Option<u64>,
        queue_metrics: Option<HeartbeatIncidentQueueMetrics>,
    ) {
        let (event, prelude) = {
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
                DiagnosticKind::FullSnapshotFrame => state.counters.full_snapshot_frames += 1,
                DiagnosticKind::TelemetryFrame => state.counters.telemetry_frames += 1,
                DiagnosticKind::TelemetryNegotiated => {
                    state.counters.telemetry_negotiations = state.counters.telemetry_negotiations.saturating_add(1)
                }
                DiagnosticKind::ResumeStarted => state.counters.resume_attempts += 1,
                DiagnosticKind::ResumeSucceeded => state.counters.resume_succeeded += 1,
                DiagnosticKind::ResumeFailed => state.counters.resume_failed += 1,
                DiagnosticKind::SignalingLeaseLost => state.counters.signaling_lease_lost += 1,
                DiagnosticKind::DisconnectRequested => state.counters.disconnect_requested += 1,
                DiagnosticKind::SessionStarted
                | DiagnosticKind::ConnectionState
                | DiagnosticKind::SessionEnded
                | DiagnosticKind::ApiFrame => {}
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

            let event = DiagnosticEvent {
                timestamp_utc: event_utc_now(),
                process_instance_id: Some(self.process_instance_id),
                monotonic_ms: now_ms,
                kind,
                phase: None,
                room_id,
                request_id,
                generation,
                transport: safe_transport(transport).map(str::to_owned),
                candidate_pair: candidate_pair.and_then(candidate_pair_types),
                payload_bytes: None,
                heartbeat_ack_latency_ms,
            };
            push_event(&mut state, event.clone());
            let prelude = (kind == DiagnosticKind::HeartbeatTimeout).then(|| state.events.iter().cloned().collect());
            (event, prelude)
        };
        self.heartbeat_incidents.record_event(&event, prelude, queue_metrics);
    }

    pub fn record_state_frame(
        &self,
        kind: DiagnosticKind,
        payload_bytes: u64,
        room_id: Option<Uuid>,
        generation: Option<u64>,
        transport: Option<&str>,
        candidate_pair: Option<&str>,
    ) {
        if !matches!(kind, DiagnosticKind::FullSnapshotFrame | DiagnosticKind::TelemetryFrame) {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        match kind {
            DiagnosticKind::FullSnapshotFrame => {
                state.counters.full_snapshot_frames = state.counters.full_snapshot_frames.saturating_add(1);
                state.counters.full_snapshot_payload_bytes =
                    state.counters.full_snapshot_payload_bytes.saturating_add(payload_bytes);
            }
            DiagnosticKind::TelemetryFrame => {
                state.counters.telemetry_frames = state.counters.telemetry_frames.saturating_add(1);
                state.counters.telemetry_payload_bytes =
                    state.counters.telemetry_payload_bytes.saturating_add(payload_bytes);
                state.last_contact_update_ms = Some(now_ms);
            }
            _ => return,
        }
        let event = DiagnosticEvent {
            timestamp_utc: event_utc_now(),
            process_instance_id: Some(self.process_instance_id),
            monotonic_ms: now_ms,
            kind,
            phase: None,
            room_id,
            request_id: None,
            generation,
            transport: safe_transport(transport).map(str::to_owned),
            candidate_pair: candidate_pair.and_then(candidate_pair_types),
            payload_bytes: Some(payload_bytes),
            heartbeat_ack_latency_ms: None,
        };
        push_event(&mut state, event.clone());
        drop(state);
        self.heartbeat_incidents.record_event(&event, None, None);
    }

    pub fn record_api_frame(&self, direction: ApiTrafficDirection, category: ApiTrafficCategory, payload_bytes: u64) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        state.api_traffic_started_ms.get_or_insert(now_ms);
        let counters = state.counters.api_traffic.category_mut(category);
        let direction_counters = match direction {
            ApiTrafficDirection::Tx => &mut counters.tx,
            ApiTrafficDirection::Rx => &mut counters.rx,
        };
        direction_counters.messages = direction_counters.messages.saturating_add(1);
        direction_counters.payload_bytes = direction_counters.payload_bytes.saturating_add(payload_bytes);
        let room_id = state.current_room_id;
        let generation = state.current_generation;
        let event = DiagnosticEvent {
            timestamp_utc: event_utc_now(),
            process_instance_id: Some(self.process_instance_id),
            monotonic_ms: now_ms,
            kind: DiagnosticKind::ApiFrame,
            phase: Some(format!("{}:{}", direction.label(), category.label())),
            room_id,
            request_id: None,
            generation,
            transport: None,
            candidate_pair: None,
            payload_bytes: Some(payload_bytes),
            heartbeat_ack_latency_ms: None,
        };
        push_event(&mut state, event.clone());
        drop(state);
        self.heartbeat_incidents.record_event(&event, None, None);
    }

    /// Merge a bounded frontend batch of receive-to-render timings, all measured with the
    /// browser's local monotonic clock. No individual sample or frame content is retained.
    pub fn record_frontend_render_batch(&self, samples: u64, total_ms: u64, max_ms: u64) {
        if samples == 0 {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.counters.telemetry_render_samples = state.counters.telemetry_render_samples.saturating_add(samples);
        state.counters.telemetry_render_total_ms = state.counters.telemetry_render_total_ms.saturating_add(total_ms);
        state.counters.telemetry_render_max_ms = state.counters.telemetry_render_max_ms.max(max_ms);
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
        let event = DiagnosticEvent {
            timestamp_utc: event_utc_now(),
            process_instance_id: Some(self.process_instance_id),
            monotonic_ms: now_ms,
            kind: DiagnosticKind::ConnectionState,
            phase: Some(safe_phase.to_owned()),
            room_id,
            request_id: None,
            generation,
            transport: None,
            candidate_pair: None,
            payload_bytes: None,
            heartbeat_ack_latency_ms: None,
        };
        push_event(&mut state, event.clone());
        drop(state);
        self.heartbeat_incidents.record_event(&event, None, None);
    }

    pub fn snapshot(&self) -> DiagnosticSnapshot {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let mut counters = state.counters.clone();
        let api_traffic_observed_ms = state
            .api_traffic_started_ms
            .map_or(0, |start| now_ms.saturating_sub(start));
        update_average_api_rates(&mut counters.api_traffic, api_traffic_observed_ms);
        let runtime_coverage_start_utc = state.events.iter().map(|event| event.timestamp_utc.as_str()).min();
        let runtime_coverage_end_utc = state.events.iter().map(|event| event.timestamp_utc.as_str()).max();
        let retained_event_count = state.events.len() as u64;
        DiagnosticSnapshot {
            counters,
            events: state.events.iter().cloned().collect(),
            current_room_id: state.current_room_id,
            current_generation: state.current_generation,
            last_heartbeat_age_ms: state.last_heartbeat_ms.map(|last| now_ms.saturating_sub(last)),
            contact_update_age_ms: state.last_contact_update_ms.map(|last| now_ms.saturating_sub(last)),
            observed_ms: now_ms,
            api_traffic_observed_ms,
            process_instance_id: Some(self.process_instance_id),
            monotonic_clock_origin_utc: Some(self.started_utc.clone()),
            runtime_coverage_start_utc: runtime_coverage_start_utc.map(str::to_owned),
            runtime_coverage_end_utc: runtime_coverage_end_utc.map(str::to_owned),
            retained_event_count,
            exported_event_count: retained_event_count,
            events_overwritten: state.events_overwritten,
            events_dropped: 0,
            events_excluded_by_time_range: 0,
            events_unknown_timestamp: 0,
            counters_scope: Some("since_last_reset".to_owned()),
        }
    }

    pub fn clear_events_and_counters(&self) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.counters = DiagnosticCounters::default();
        state.events.clear();
        state.api_traffic_started_ms = None;
    }
}

fn update_average_api_rates(counters: &mut ApiTrafficCounters, observed_ms: u64) {
    fn update(direction: &mut ApiTrafficDirectionCounters, observed_ms: u64) {
        if observed_ms == 0 {
            direction.average_messages_per_sec_milli_since_window_start = 0;
            direction.average_payload_bytes_per_sec_milli_since_window_start = 0;
        } else {
            direction.average_messages_per_sec_milli_since_window_start =
                ((direction.messages as u128).saturating_mul(1_000_000) / observed_ms as u128).min(u64::MAX as u128)
                    as u64;
            direction.average_payload_bytes_per_sec_milli_since_window_start =
                ((direction.payload_bytes as u128).saturating_mul(1_000_000) / observed_ms as u128)
                    .min(u64::MAX as u128) as u64;
        }
    }
    for category in [
        &mut counters.heartbeat,
        &mut counters.state,
        &mut counters.command,
        &mut counters.authentication,
        &mut counters.session,
        &mut counters.other,
    ] {
        update(&mut category.tx, observed_ms);
        update(&mut category.rx, observed_ms);
    }
}

fn push_event(state: &mut RecorderState, event: DiagnosticEvent) {
    if state.events.len() == MAX_DIAGNOSTIC_EVENTS {
        state.events.pop_front();
        state.events_overwritten = state.events_overwritten.saturating_add(1);
    }
    state.events.push_back(event);
}

fn event_utc_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(chrono::DateTime::parse_from_rfc3339(value["events"][0]["timestamp_utc"].as_str().unwrap()).is_ok());
        assert_eq!(value["events"][0]["process_instance_id"], value["process_instance_id"]);
        assert!(value["monotonic_clock_origin_utc"].as_str().is_some());
        let serialized = value.to_string();
        assert!(!serialized.contains("192.168.1.2"));
        assert!(!serialized.contains("8.8.8.8"));
        assert!(serialized.contains("RELAY/UDP↔RELAY/UDP"));
    }

    #[test]
    fn diagnostic_event_accepts_legacy_json_without_heartbeat_ack_latency() {
        let legacy = serde_json::json!({
            "timestamp_utc": "2026-10-24T22:43:44.111Z",
            "process_instance_id": Uuid::new_v4(),
            "monotonic_ms": 1200,
            "kind": "heartbeat_ack",
            "phase": null,
            "room_id": Uuid::new_v4(),
            "request_id": Uuid::new_v4(),
            "generation": 4,
            "transport": "webrtc_datachannel",
            "candidate_pair": "RELAY/UDP↔RELAY/UDP",
            "payload_bytes": null
        });
        let parsed: DiagnosticEvent = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.kind, DiagnosticKind::HeartbeatAck);
        assert_eq!(parsed.heartbeat_ack_latency_ms, None);
    }

    #[test]
    fn telemetry_metrics_retain_only_bounded_aggregates_and_numeric_frame_sizes() {
        let recorder = DiagnosticRecorder::default();
        let room_id = Uuid::new_v4();
        recorder.record_state_frame(
            DiagnosticKind::FullSnapshotFrame,
            1_500,
            Some(room_id),
            Some(2),
            Some("webrtc_datachannel"),
            Some("RELAY/UDP↔RELAY/UDP"),
        );
        recorder.record_state_frame(
            DiagnosticKind::TelemetryFrame,
            420,
            Some(room_id),
            Some(2),
            Some("webrtc_datachannel"),
            Some("RELAY/UDP↔RELAY/UDP"),
        );
        recorder.record_frontend_render_batch(12, 30, 6);
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.counters.full_snapshot_frames, 1);
        assert_eq!(snapshot.counters.telemetry_frames, 1);
        assert_eq!(snapshot.counters.full_snapshot_payload_bytes, 1_500);
        assert_eq!(snapshot.counters.telemetry_payload_bytes, 420);
        assert_eq!(snapshot.counters.telemetry_render_samples, 12);
        assert_eq!(snapshot.counters.telemetry_render_total_ms, 30);
        assert_eq!(snapshot.counters.telemetry_render_max_ms, 6);
        assert!(snapshot.contact_update_age_ms.is_some());
        let value = serde_json::to_value(&snapshot).unwrap();
        let serialized = value.to_string();
        assert!(!serialized.contains("credentials"));
        let events = value["events"].as_array().unwrap();
        assert_eq!(events[0]["payload_bytes"], 1_500);
        assert!(events.iter().all(|event| event.get("payload").is_none()));
    }

    #[test]
    fn api_traffic_metrics_aggregate_only_category_direction_and_size() {
        let recorder = DiagnosticRecorder::default();
        recorder.record_api_frame(ApiTrafficDirection::Tx, ApiTrafficCategory::Heartbeat, 87);
        recorder.record_api_frame(ApiTrafficDirection::Rx, ApiTrafficCategory::Heartbeat, 96);
        recorder.record_api_frame(ApiTrafficDirection::Tx, ApiTrafficCategory::Command, 512);
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.counters.api_traffic.heartbeat.tx.messages, 1);
        assert_eq!(snapshot.counters.api_traffic.heartbeat.tx.payload_bytes, 87);
        assert_eq!(snapshot.counters.api_traffic.heartbeat.rx.messages, 1);
        assert_eq!(snapshot.counters.api_traffic.heartbeat.rx.payload_bytes, 96);
        assert_eq!(snapshot.counters.api_traffic.command.tx.messages, 1);
        assert_eq!(snapshot.counters.api_traffic.command.tx.payload_bytes, 512);
        assert!(snapshot.observed_ms >= snapshot.events.last().unwrap().monotonic_ms);
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains("nonce-value"));
        assert!(!serialized.contains("secret-frame-content"));
        assert!(serialized.contains("tx:heartbeat"));
        assert!(serialized.contains("payload_bytes"));
    }

    #[test]
    fn api_traffic_average_rates_use_monotonic_observation_duration() {
        let mut counters = ApiTrafficCounters::default();
        counters.heartbeat.tx.messages = 4;
        counters.heartbeat.tx.payload_bytes = 800;
        update_average_api_rates(&mut counters, 2_000);
        assert_eq!(counters.heartbeat.tx.average_messages_per_sec_milli_since_window_start, 2_000);
        assert_eq!(
            counters
                .heartbeat
                .tx
                .average_payload_bytes_per_sec_milli_since_window_start,
            400_000
        );
        update_average_api_rates(&mut counters, 0);
        assert_eq!(counters.heartbeat.tx.average_messages_per_sec_milli_since_window_start, 0);
        assert_eq!(
            counters
                .heartbeat
                .tx
                .average_payload_bytes_per_sec_milli_since_window_start,
            0
        );
    }

    #[test]
    fn event_ring_is_bounded() {
        let recorder = DiagnosticRecorder::default();
        for _ in 0..(MAX_DIAGNOSTIC_EVENTS + 7) {
            recorder.record(DiagnosticKind::HeartbeatTx, None, None, None, None, None);
        }
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.events.len(), MAX_DIAGNOSTIC_EVENTS);
        assert_eq!(snapshot.retained_event_count, MAX_DIAGNOSTIC_EVENTS as u64);
        assert_eq!(snapshot.exported_event_count, MAX_DIAGNOSTIC_EVENTS as u64);
        assert_eq!(snapshot.events_overwritten, 7);
        assert_eq!(snapshot.events_dropped, 0);
        assert!(snapshot.runtime_coverage_start_utc.is_some());
        assert!(snapshot.runtime_coverage_end_utc.is_some());
        assert!(
            snapshot
                .events
                .iter()
                .all(|event| event.process_instance_id == snapshot.process_instance_id)
        );
        assert!(snapshot.events.iter().all(|event| !event.timestamp_utc.is_empty()));
    }

    #[test]
    fn heartbeat_incident_survives_runtime_ring_overwrite_and_process_restart() {
        let path = std::env::temp_dir().join(format!("shockingvrc-heartbeat-recorder-{}.json", Uuid::new_v4()));
        let recorder = DiagnosticRecorder::open(&path).unwrap();
        let room_id = Uuid::new_v4();
        recorder.record(
            DiagnosticKind::HeartbeatTx,
            Some(room_id),
            Some(Uuid::new_v4()),
            Some(4),
            Some("webrtc_datachannel"),
            Some("RELAY/UDP 192.168.1.5 ↔ RELAY/UDP 8.8.8.8"),
        );
        recorder.record_heartbeat_ack(
            Some(room_id),
            Some(Uuid::new_v4()),
            Some(4),
            Some("webrtc_datachannel"),
            Some("RELAY/UDP 192.168.1.5 ↔ RELAY/UDP 8.8.8.8"),
            42,
        );
        recorder.record_heartbeat_timeout(
            Some(room_id),
            Some(4),
            Some("webrtc_datachannel"),
            Some("RELAY/UDP 192.168.1.5 ↔ RELAY/UDP 8.8.8.8"),
            HeartbeatIncidentQueueMetrics {
                action_queue_depth: 1,
                action_queue_capacity: 8,
                pending_heartbeat_requests: 2,
                data_channel_sender_available: Some(true),
                ice_pair_available: true,
                ..HeartbeatIncidentQueueMetrics::default()
            },
        );
        recorder.record(
            DiagnosticKind::ResumeStarted,
            Some(room_id),
            None,
            Some(4),
            Some("webrtc_datachannel"),
            None,
        );
        recorder.record(
            DiagnosticKind::ResumeSucceeded,
            Some(room_id),
            None,
            Some(4),
            Some("webrtc_datachannel"),
            None,
        );
        recorder.record_connection_phase("online");
        for _ in 0..(MAX_DIAGNOSTIC_EVENTS + 20) {
            recorder.record(DiagnosticKind::ApiFrame, Some(room_id), None, Some(4), None, None);
        }
        assert!(recorder.snapshot().events_overwritten > 0);
        let incidents = recorder.heartbeat_incidents();
        incidents.flush().unwrap();
        let current = incidents.snapshot();
        assert_eq!(current.len(), 1);
        assert_eq!(
            current[0].outcome,
            crate::heartbeat_incidents::HeartbeatIncidentOutcome::Recovered
        );
        assert!(
            current[0]
                .prelude_events
                .iter()
                .any(|event| event.heartbeat_ack_latency_ms == Some(42))
        );
        assert!(
            current[0]
                .recovery_events
                .iter()
                .any(|event| event.kind == DiagnosticKind::ResumeSucceeded)
        );
        drop(incidents);
        drop(recorder);

        let reopened = DiagnosticRecorder::open(&path).unwrap();
        let persisted = reopened.heartbeat_incidents().snapshot();
        assert_eq!(persisted.len(), 1);
        assert_eq!(
            persisted[0].outcome,
            crate::heartbeat_incidents::HeartbeatIncidentOutcome::Recovered
        );
        let encoded = serde_json::to_string(&persisted).unwrap();
        assert!(encoded.contains("heartbeat_timeout"));
        assert!(!encoded.contains("192.168.1.5"));
        assert!(!encoded.contains("8.8.8.8"));
        assert!(!encoded.contains("access_token"));
        reopened.heartbeat_incidents().flush().unwrap();
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }
}
