//! Host Remote API v1 server and transport-neutral protocol types.

use thiserror::Error;

pub mod api;
pub mod auth;
pub mod controller;
pub mod identity;
pub mod operations;
pub mod protocol;
pub mod settings;
pub mod tls;

pub use controller::{HostRemoteController, HostRemoteStatus, ListenerState, TraversalState};
pub use identity::{HostIdentity, IdentityError};
pub use protocol::{RemoteCommand, SnapshotDto};

pub use operations::{REMOTE_OPERATIONS, is_remote_operation};
pub use settings::{
    ChannelCaps, RemoteAccessSettings, RemoteChannel, RemoteLimitOperation, RemoteStrengthPolicy, SettingsError,
    StrengthPolicyError,
};

pub trait RemoteBackend: Send + Sync + 'static {
    fn snapshot(&self) -> Result<SnapshotDto, BackendError>;
    fn presets(&self) -> Result<protocol::PresetCatalogDto, BackendError>;
    fn zone_presets(&self) -> Result<protocol::ZonePresetCatalogDto, BackendError>;
    fn logs(&self, cursor: Option<u64>, limit: u16) -> Result<protocol::LogPageDto, BackendError>;
    fn enqueue(&self, command: RemoteCommand) -> Result<(), BackendError>;
}

#[derive(Debug, Error)]
#[error("{code}: {message}")]
pub struct BackendError {
    pub code: &'static str,
    pub message: String,
}

impl BackendError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
