use std::path::PathBuf;

use tauri::ipc::Channel;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    protocol::{CapabilitiesDto, ConnectionStateDto, SnapshotDto, UiEvent},
    storage::ProfileStore,
    transport::SessionAction,
};
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
    snapshot: Option<SnapshotDto>,
    capabilities: Option<CapabilitiesDto>,
    pairing_profile: Option<crate::protocol::ProfileDto>,
    #[cfg(test)]
    connection_phases: Vec<String>,
}

#[derive(Default)]
pub struct UiBridge {
    channel: Mutex<Option<Channel<UiEvent>>>,
    latest: Mutex<LatestUiState>,
}

impl UiBridge {
    pub async fn publish(&self, event: UiEvent) {
        {
            let mut latest = self.latest.lock().await;
            match &event {
                UiEvent::Connection(value) => {
                    latest.connection = Some(value.clone());
                    #[cfg(test)]
                    latest.connection_phases.push(value.phase.clone());
                    if value.phase != "online" && value.phase != "reconnecting" {
                        latest.snapshot = None;
                        latest.capabilities = None;
                    }
                }
                UiEvent::PairingInvitation(value) => latest.pairing_profile = Some(value.clone()),
                UiEvent::Snapshot(value) => latest.snapshot = Some(value.clone()),
                UiEvent::Capabilities(value) => latest.capabilities = Some(value.clone()),
                UiEvent::Catalog(_) | UiEvent::Logs(_) | UiEvent::Error(_) => {}
            }
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
            let _ = channel.send(UiEvent::Connection(ConnectionStateDto {
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
            }));
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

    #[cfg(test)]
    pub async fn latest_connection_for_test(&self) -> Option<ConnectionStateDto> {
        self.latest.lock().await.connection.clone()
    }

    #[cfg(test)]
    pub async fn latest_snapshot_for_test(&self) -> Option<SnapshotDto> {
        self.latest.lock().await.snapshot.clone()
    }

    #[cfg(test)]
    pub async fn connection_phases_for_test(&self) -> Vec<String> {
        self.latest.lock().await.connection_phases.clone()
    }
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
