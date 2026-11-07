use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::ipc::Channel;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    diagnostics::DiagnosticRecorder,
    protocol::{CapabilitiesDto, ConnectionStateDto, SnapshotEventDto, UiEvent},
    storage::ProfileStore,
    transport::SessionAction,
};
#[cfg(test)]
use crate::protocol::SnapshotContextDto;
use shocking_vrc_rendezvous::pairing::PairingInvitationV1;

pub struct ConnectionHandle {
    pub id: uuid::Uuid,
    pub profile_id: String,
    pub actions: mpsc::Sender<SessionAction>,
    pub cancel: CancellationToken,
    pub closed: Option<oneshot::Receiver<()>>,
}

pub struct AppState {
    pub terminal_log_path: PathBuf,
    pub profiles: Mutex<ProfileStore>,
    pub pending_pairing: Mutex<Option<PairingInvitationV1>>,
    pub pending_pairing_profile: Mutex<Option<crate::protocol::ProfileDto>>,
    pub connection: std::sync::Arc<Mutex<Option<ConnectionHandle>>>,
    pub connection_lifecycle: Mutex<()>,
    pub updates: std::sync::Arc<UiBridge>,
}

#[derive(Default)]
struct LatestUiState {
    connection: Option<ConnectionStateDto>,
    snapshot: Option<SnapshotEventDto>,
    capabilities: Option<CapabilitiesDto>,
    pairing_profile: Option<crate::protocol::ProfileDto>,
    #[cfg(test)]
    connection_phases: Vec<String>,
}

#[derive(Default)]
pub struct UiBridge {
    channel: Mutex<Option<Channel<UiEvent>>>,
    latest: Mutex<LatestUiState>,
    next_session_generation: AtomicU64,
    pub diagnostics: DiagnosticRecorder,
}

impl UiBridge {
    pub fn begin_session(&self) -> u64 {
        self.next_session_generation.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }

    pub async fn publish(&self, event: UiEvent) {
        let event = match event {
            UiEvent::Connection(mut value) => {
                if value.phase == "disconnected" && value.session_generation.is_none() {
                    let generation = self.next_session_generation.load(Ordering::Relaxed);
                    value.session_generation = (generation > 0).then_some(generation);
                }
                UiEvent::Connection(value)
            }
            other => other,
        };
        let mut forward = true;
        {
            let mut latest = self.latest.lock().await;
            match &event {
                UiEvent::Connection(value) => {
                    let highest_generation = self.next_session_generation.load(Ordering::Relaxed);
                    if latest.connection.as_ref().is_some_and(|current| connection_event_is_stale(current, value, highest_generation)) {
                        forward = false;
                    } else {
                        let changed_context = latest.connection.as_ref().is_some_and(|current| !same_connection_context(current, value));
                        if changed_context {
                            latest.snapshot = None;
                            latest.capabilities = None;
                        }
                        latest.connection = Some(value.clone());
                        #[cfg(test)]
                        latest.connection_phases.push(value.phase.clone());
                        if value.phase != "online" && value.phase != "reconnecting" {
                            latest.snapshot = None;
                            latest.capabilities = None;
                        }
                    }
                }
                UiEvent::PairingInvitation(value) => latest.pairing_profile = Some(value.clone()),
                UiEvent::Snapshot(value) => {
                    let active = latest.connection.as_ref().is_some_and(|connection| {
                        matches!(connection.phase.as_str(), "online" | "reconnecting")
                            && connection.session_generation == Some(value.context.session_generation)
                            && connection.server_epoch.as_deref() == Some(value.context.server_epoch.as_str())
                            && connection.transport_generation == Some(value.context.transport_generation)
                    });
                    let newer = latest.snapshot.as_ref().is_none_or(|current| {
                        current.context.session_generation != value.context.session_generation
                            || current.context.server_epoch != value.context.server_epoch
                            || current.context.transport_generation != value.context.transport_generation
                            || value.context.seq > current.context.seq
                    });
                    if active && newer {
                        latest.snapshot = Some(value.clone());
                    } else {
                        forward = false;
                    }
                }
                UiEvent::Capabilities(value) => latest.capabilities = Some(value.clone()),
                UiEvent::Catalog(_) | UiEvent::Logs(_) | UiEvent::Error(_) => {}
            }
        }
        if !forward { return; }
        if let UiEvent::Connection(value) = &event {
            self.diagnostics.record_connection_phase(&value.phase);
        }
        if let Some(channel) = self.channel.lock().await.as_ref() {
            let _ = channel.send(event);
        }
    }

    pub async fn attach(&self, channel: Channel<UiEvent>) {
        *self.channel.lock().await = Some(channel.clone());
        let latest = self.latest.lock().await;
        if let Some(connection) = &latest.connection {
            let _ = channel.send(UiEvent::Connection(connection.clone()));
        } else {
            let _ = channel.send(UiEvent::Connection(disconnected_connection_state()));
        }
        if let Some(profile) = &latest.pairing_profile { let _ = channel.send(UiEvent::PairingInvitation(profile.clone())); }
        if let Some(snapshot) = &latest.snapshot {
            let _ = channel.send(UiEvent::Snapshot(snapshot.clone()));
        }
        if let Some(capabilities) = &latest.capabilities {
            let _ = channel.send(UiEvent::Capabilities(capabilities.clone()));
        }
    }

    pub async fn clear_pairing_profile(&self, profile_id: &str) {
        let mut latest = self.latest.lock().await;
        if latest.pairing_profile.as_ref().is_some_and(|profile| profile.id == profile_id) {
            latest.pairing_profile = None;
        }
    }

    pub async fn clear_connection_profile(&self, profile_id: &str) -> bool {
        let event = {
            let mut latest = self.latest.lock().await;
            let Some(connection) = latest.connection.as_ref() else { return false };
            let is_active = matches!(
                connection.phase.as_str(),
                "connecting" | "identity_verified" | "online" | "reconnecting"
            );
            if connection.profile_id.as_deref() != Some(profile_id) || is_active {
                return false;
            }
            let mut disconnected = disconnected_connection_state();
            let generation = self.next_session_generation.load(Ordering::Relaxed);
            disconnected.session_generation = (generation > 0).then_some(generation);
            latest.connection = Some(disconnected.clone());
            latest.snapshot = None;
            latest.capabilities = None;
            #[cfg(test)]
            latest.connection_phases.push(disconnected.phase.clone());
            self.diagnostics.record_connection_phase(&disconnected.phase);
            UiEvent::Connection(disconnected)
        };
        if let Some(channel) = self.channel.lock().await.as_ref() {
            let _ = channel.send(event);
        }
        true
    }

    #[cfg(test)]
    pub async fn latest_connection_for_test(&self) -> Option<ConnectionStateDto> {
        self.latest.lock().await.connection.clone()
    }

    #[cfg(test)]
    pub async fn latest_snapshot_for_test(&self) -> Option<SnapshotEventDto> {
        self.latest.lock().await.snapshot.clone()
    }

    #[cfg(test)]
    pub async fn connection_phases_for_test(&self) -> Vec<String> {
        self.latest.lock().await.connection_phases.clone()
    }
}

fn disconnected_connection_state() -> ConnectionStateDto {
    ConnectionStateDto {
        phase: "disconnected".to_owned(),
        transport: None,
        ice_candidate_pair: None,
        ice_servers_configured: None,
        rendezvous_stun_received: None,
        turn_credentials_received: None,
        local_candidates_gathered: None,
        profile_id: None,
        host_label: None,
        fingerprint: None,
        server_id: None,
        last_heartbeat: None,
        message: None,
        session_generation: None,
        server_epoch: None,
        transport_generation: None,
    }
}

fn same_connection_context(left: &ConnectionStateDto, right: &ConnectionStateDto) -> bool {
    left.session_generation == right.session_generation
        && left.server_epoch == right.server_epoch
        && left.transport_generation == right.transport_generation
}

fn connection_event_is_stale(current: &ConnectionStateDto, incoming: &ConnectionStateDto, highest_generation: u64) -> bool {
    let incoming_active = matches!(incoming.phase.as_str(), "connecting" | "identity_verified" | "online" | "reconnecting");
    let current_active = matches!(current.phase.as_str(), "connecting" | "identity_verified" | "online" | "reconnecting");
    if incoming_active && incoming.session_generation.is_none() { return true; }
    if incoming.session_generation.is_some_and(|generation| generation < highest_generation) { return true; }
    if !current_active && incoming_active {
        match (current.session_generation, incoming.session_generation) {
            (Some(current), Some(incoming)) if incoming <= current => return true,
            (Some(_), None) => return true,
            (None, Some(incoming)) if incoming < highest_generation => return true,
            (None, None) => return true,
            _ => {}
        }
    }
    match (current.session_generation, incoming.session_generation) {
        (Some(current), Some(incoming)) if incoming < current => return true,
        (Some(current), Some(incoming)) if incoming > current => return false,
        _ => {}
    }
    current.server_epoch == incoming.server_epoch
        && matches!((current.transport_generation, incoming.transport_generation), (Some(current), Some(incoming)) if incoming < current)
}

impl AppState {
    pub fn open(data_dir: PathBuf) -> Result<Self, String> {
        let profiles = ProfileStore::open(data_dir.clone())?;
        Ok(Self {
            terminal_log_path: data_dir.join("transport.log"),
            profiles: Mutex::new(profiles),
            pending_pairing: Mutex::new(None),
            pending_pairing_profile: Mutex::new(None),
            connection: std::sync::Arc::new(Mutex::new(None)),
            connection_lifecycle: Mutex::new(()),
            updates: std::sync::Arc::new(UiBridge::default()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(phase: &str, profile_id: &str) -> ConnectionStateDto {
        ConnectionStateDto {
            phase: phase.to_owned(),
            transport: None,
            ice_candidate_pair: None,
            ice_servers_configured: None,
            rendezvous_stun_received: None,
            turn_credentials_received: None,
            local_candidates_gathered: None,
            profile_id: Some(profile_id.to_owned()),
            host_label: Some("Saved Host".to_owned()),
            fingerprint: Some("ab".repeat(32)),
            server_id: Some("host-id".to_owned()),
            last_heartbeat: None,
            message: Some("test state".to_owned()),
            session_generation: Some(1),
            server_epoch: Some("epoch-a".to_owned()),
            transport_generation: Some(1),
        }
    }

    fn snapshot_event(seq: u64, session_generation: u64, server_epoch: &str, transport_generation: u64) -> SnapshotEventDto {
        let state = serde_json::from_value(serde_json::json!({
            "bridge_revision": seq,
            "active_device": 0,
            "devices": [],
            "vrchat": { "connected": false, "osc_port": 0, "oscquery_port": null },
            "avatar_zones": [],
            "config": {
                "channel_a": { "zones": [], "frequency": [0,0,0,0], "intensity": [0,0,0,0], "limits": {"min": 0, "max": 30}, "aggregation": "Max", "freq_modulation": [null,null,null,null], "intensity_modulation": [null,null,null,null] },
                "channel_b": { "zones": [], "frequency": [0,0,0,0], "intensity": [0,0,0,0], "limits": {"min": 0, "max": 30}, "aggregation": "Max", "freq_modulation": [null,null,null,null], "intensity_modulation": [null,null,null,null] },
                "ukf": {}, "norms": {}
            },
            "status": {}, "alarm": {},
            "remote_policy": { "remote_strength": {"enabled": false, "max_by_channel": {"A": null, "B": null}, "scope": "channel_power_limits_min_max"}, "max_remote_sessions": 1 }
        })).expect("valid snapshot state");
        SnapshotEventDto {
            state,
            context: SnapshotContextDto {
                seq,
                server_epoch: server_epoch.to_owned(),
                transport_generation,
                session_generation,
            },
        }
    }

    #[tokio::test]
    async fn deleting_profile_clears_ended_connection_identity_but_preserves_active_session() {
        let updates = UiBridge::default();
        updates.publish(UiEvent::Connection(connection("session_revoked", "host-a"))).await;

        assert!(updates.clear_connection_profile("host-a").await);
        let cleared = updates.latest_connection_for_test().await.expect("cleared connection");
        assert_eq!(cleared.phase, "disconnected");
        assert_eq!(cleared.profile_id, None);
        assert_eq!(cleared.fingerprint, None);

        updates.publish(UiEvent::Connection(connection("online", "host-b"))).await;
        assert!(!updates.clear_connection_profile("host-b").await);
        assert_eq!(updates.latest_connection_for_test().await.unwrap().phase, "online");
    }

    #[tokio::test]
    async fn snapshots_require_current_owner_epoch_generation_and_increasing_sequence() {
        let updates = UiBridge::default();
        updates.publish(UiEvent::Connection(connection("online", "host-a"))).await;
        updates.publish(UiEvent::Snapshot(snapshot_event(8, 1, "epoch-a", 1))).await;
        updates.publish(UiEvent::Snapshot(snapshot_event(8, 1, "epoch-a", 1))).await;
        updates.publish(UiEvent::Snapshot(snapshot_event(7, 1, "epoch-a", 1))).await;
        assert_eq!(updates.latest_snapshot_for_test().await.unwrap().context.seq, 8);

        let mut resumed = connection("online", "host-a");
        resumed.transport_generation = Some(2);
        updates.publish(UiEvent::Connection(resumed)).await;
        assert!(updates.latest_snapshot_for_test().await.is_none());
        updates.publish(UiEvent::Snapshot(snapshot_event(1, 1, "epoch-a", 2))).await;
        assert_eq!(updates.latest_snapshot_for_test().await.unwrap().context.seq, 1);
        updates.publish(UiEvent::Snapshot(snapshot_event(99, 1, "epoch-a", 1))).await;
        assert_eq!(updates.latest_snapshot_for_test().await.unwrap().context.transport_generation, 2);
    }

    #[tokio::test]
    async fn old_session_events_cannot_replace_a_new_session_context() {
        let updates = UiBridge::default();
        let mut current = connection("online", "host-a");
        current.session_generation = Some(2);
        current.server_epoch = Some("epoch-b".to_owned());
        updates.publish(UiEvent::Connection(current)).await;
        updates.publish(UiEvent::Connection(connection("online", "host-a"))).await;
        assert_eq!(updates.latest_connection_for_test().await.unwrap().session_generation, Some(2));
        updates.publish(UiEvent::Snapshot(snapshot_event(900, 1, "epoch-a", 1))).await;
        assert!(updates.latest_snapshot_for_test().await.is_none());
        updates.publish(UiEvent::Snapshot(snapshot_event(1, 2, "epoch-b", 1))).await;
        assert_eq!(updates.latest_snapshot_for_test().await.unwrap().context.server_epoch, "epoch-b");
    }

    #[tokio::test]
    async fn ended_session_generation_cannot_be_resurrected_by_a_late_connection_event() {
        let updates = UiBridge::default();
        let generation = updates.begin_session();
        assert_eq!(generation, 1);
        let mut online = connection("online", "host-a");
        online.session_generation = Some(generation);
        updates.publish(UiEvent::Connection(online.clone())).await;
        updates.publish(UiEvent::Connection(disconnected_connection_state())).await;

        updates.publish(UiEvent::Connection(online)).await;
        assert_eq!(updates.latest_connection_for_test().await.unwrap().phase, "disconnected");

        let next_generation = updates.begin_session();
        let mut connecting = connection("connecting", "host-a");
        connecting.session_generation = Some(next_generation);
        updates.publish(UiEvent::Connection(connecting)).await;
        assert_eq!(updates.latest_connection_for_test().await.unwrap().session_generation, Some(next_generation));
    }
}
