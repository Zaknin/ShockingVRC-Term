use std::path::PathBuf;

use tauri::ipc::Channel;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    protocol::{CapabilitiesDto, ConnectionStateDto, SnapshotDto, UiEvent},
    storage::ProfileStore,
    transport::{PendingConnection, SessionAction},
};

pub struct ConnectionHandle {
    pub id: uuid::Uuid,
    pub profile_id: String,
    pub actions: mpsc::Sender<SessionAction>,
    pub cancel: CancellationToken,
}

pub struct AppState {
    pub terminal_log_path: PathBuf,
    pub profiles: Mutex<ProfileStore>,
    pub pending: Mutex<Option<PendingConnection>>,
    pub connection: std::sync::Arc<Mutex<Option<ConnectionHandle>>>,
    pub updates: std::sync::Arc<UiBridge>,
}

#[derive(Default)]
struct LatestUiState {
    connection: Option<ConnectionStateDto>,
    snapshot: Option<SnapshotDto>,
    capabilities: Option<CapabilitiesDto>,
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
                UiEvent::Connection(value) => latest.connection = Some(value.clone()),
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
                profile_id: None,
                host_label: None,
                fingerprint: None,
                server_id: None,
                last_heartbeat: None,
                message: None,
            }));
        }
        if let Some(snapshot) = &latest.snapshot {
            let _ = channel.send(UiEvent::Snapshot(snapshot.clone()));
        }
        if let Some(capabilities) = &latest.capabilities {
            let _ = channel.send(UiEvent::Capabilities(capabilities.clone()));
        }
    }
}

impl AppState {
    pub fn open(data_dir: PathBuf) -> Result<Self, String> {
        let profiles = ProfileStore::open(data_dir.clone())?;
        Ok(Self {
            terminal_log_path: data_dir.join("transport.log"),
            profiles: Mutex::new(profiles),
            pending: Mutex::new(None),
            connection: std::sync::Arc::new(Mutex::new(None)),
            updates: std::sync::Arc::new(UiBridge::default()),
        })
    }
}
