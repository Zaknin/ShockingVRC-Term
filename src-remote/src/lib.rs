//! Host Remote API v1 server and transport-neutral protocol types.

use thiserror::Error;

pub mod api;
pub mod auth;
pub mod controller;
pub mod diagnostics;
pub mod graph_validation;
pub mod identity;
pub mod network;
pub mod operations;
pub mod protocol;
pub mod settings;
pub mod tls;

pub use controller::{HostRemoteController, HostRemoteStatus, ListenerState, TraversalState};
pub use identity::{HostIdentity, IdentityError};
pub use network::{LocalIpv4Interface, available_local_ipv4};
pub use protocol::{RemoteCommand, SnapshotDto};

pub use operations::{REMOTE_OPERATIONS, is_remote_operation};
pub use settings::{
    ChannelCaps, InvitationMode, RemoteAccessSettings, RemoteChannel, RemoteLimitOperation, RemoteStrengthPolicy,
    SettingsError, StrengthPolicyError,
};

pub trait RemoteBackend: Send + Sync + 'static {
    fn snapshot(&self) -> Result<SnapshotDto, BackendError>;
    /// Age of the Host worker publication used to produce the next API frame, if the backend
    /// retains an internal monotonic publish timestamp.
    fn snapshot_age_ms(&self) -> Option<u64> {
        None
    }
    fn presets(&self) -> Result<protocol::PresetCatalogDto, BackendError>;
    fn zone_presets(&self) -> Result<protocol::ZonePresetCatalogDto, BackendError>;
    fn graph_supported(&self) -> bool {
        false
    }
    fn graph_document(&self) -> Result<protocol::GraphDocumentDto, BackendError> {
        Err(BackendError::new(
            "UNSUPPORTED_OPERATION",
            "Remote Graph document is unavailable",
        ))
    }
    fn graph_presets(&self) -> Result<protocol::GraphPresetCatalogDto, BackendError> {
        Err(BackendError::new(
            "UNSUPPORTED_OPERATION",
            "Remote Graph presets are unavailable",
        ))
    }
    fn graph_preset(&self, _preset_id: &str) -> Result<serde_json::Value, BackendError> {
        Err(BackendError::new(
            "UNSUPPORTED_OPERATION",
            "Remote Graph preset detail is unavailable",
        ))
    }
    fn begin_graph_apply(
        &self,
        _session_id: uuid::Uuid,
        _generation: u64,
        _apply_id: uuid::Uuid,
        _base_revision: u64,
        _graph: shocking_vrc_core::graph::Graph,
    ) -> Result<(), BackendError> {
        Err(BackendError::new("UNSUPPORTED_OPERATION", "Remote Graph apply is unavailable"))
    }
    fn graph_apply_status(
        &self,
        _session_id: uuid::Uuid,
        _generation: u64,
        _apply_id: uuid::Uuid,
    ) -> Result<protocol::GraphApplyStatusDto, BackendError> {
        Err(BackendError::new(
            "TRANSFER_NOT_FOUND",
            "Graph apply status is unavailable for this authenticated session",
        ))
    }

    fn logs(&self, cursor: Option<u64>, limit: u16) -> Result<protocol::LogPageDto, BackendError>;
    fn application_provenance(&self) -> (String, String) {
        (env!("CARGO_PKG_VERSION").to_owned(), "unknown".to_owned())
    }
    fn begin_log_export(
        &self,
        _session_id: uuid::Uuid,
        _generation: u64,
        _range: Option<shocking_vrc_diagnostics::LogTimeRange>,
    ) -> Result<shocking_vrc_diagnostics::LogSnapshotMetadata, BackendError> {
        Err(BackendError::new(
            "UNSUPPORTED_OPERATION",
            "Host diagnostic export is unavailable",
        ))
    }
    fn read_log_export_chunk(
        &self,
        _session_id: uuid::Uuid,
        _generation: u64,
        _snapshot_id: uuid::Uuid,
        _offset: u64,
        _max_bytes: usize,
    ) -> Result<shocking_vrc_diagnostics::LogExportChunk, BackendError> {
        Err(BackendError::new(
            "UNSUPPORTED_OPERATION",
            "Host diagnostic export is unavailable",
        ))
    }
    fn cancel_log_export(&self, _session_id: uuid::Uuid, _generation: u64, _snapshot_id: uuid::Uuid) {}
    /// Release every diagnostic snapshot owned by this authenticated session generation.
    fn end_log_export_session(&self, _session_id: uuid::Uuid, _generation: u64) {}
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
