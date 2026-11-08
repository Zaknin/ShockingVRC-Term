use shocking_vrc_remote::{
    BackendError, RemoteBackend, RemoteCommand,
    operations::{REMOTE_OPERATIONS, is_remote_operation, remote_operations_for},
    protocol::{LogPageDto, PresetCatalogDto, SnapshotDto, ZonePresetCatalogDto},
    protocol::{ProtocolError, parse_rest_command},
};

fn command(op: &str, args: &str) -> Result<shocking_vrc_remote::RemoteCommand, ProtocolError> {
    let body = format!(r#"{{"request_id":"00000000-0000-0000-0000-000000000001","op":"{op}","args":{args}}}"#);
    parse_rest_command(body.as_bytes())
}

#[test]
fn mode_switch_accepts_classic_and_graph_and_rejects_unknown_values() {
    assert!(command("engine.set_mode", r#"{"mode":"graph"}"#).is_ok());
    assert!(command("engine.set_mode", r#"{"mode":"classic"}"#).is_ok());
    assert!(matches!(
        command("engine.set_mode", r#"{"mode":"automatic"}"#),
        Err(ProtocolError::InvalidArguments(_))
    ));
}

#[test]
fn mode_switch_requires_exact_arguments() {
    assert!(matches!(
        command("engine.set_mode", r#"{}"#),
        Err(ProtocolError::InvalidArguments(_))
    ));
    assert!(matches!(
        command("engine.set_mode", r#"{"mode":"graph","force":true}"#),
        Err(ProtocolError::InvalidArguments(_))
    ));
}

#[test]
fn mode_switch_is_recognized_without_changing_the_legacy_operation_catalog() {
    assert_eq!(REMOTE_OPERATIONS.len(), 37);
    assert!(is_remote_operation("engine.set_mode"));
    assert!(!is_remote_operation("graph.panic.release"));
}

#[test]
fn graph_operation_is_only_advertised_to_graph_capable_backends() {
    let legacy = remote_operations_for(false);
    let graph = remote_operations_for(true);

    assert_eq!(legacy.len(), REMOTE_OPERATIONS.len());
    assert!(!legacy.contains(&"engine.set_mode"));
    assert_eq!(graph.len(), REMOTE_OPERATIONS.len() + 1);
    assert!(graph.contains(&"engine.set_mode"));
}

struct LegacyBackend;

impl RemoteBackend for LegacyBackend {
    fn snapshot(&self) -> Result<SnapshotDto, BackendError> {
        Err(BackendError::new("TEST", "unused"))
    }

    fn presets(&self) -> Result<PresetCatalogDto, BackendError> {
        Err(BackendError::new("TEST", "unused"))
    }

    fn zone_presets(&self) -> Result<ZonePresetCatalogDto, BackendError> {
        Err(BackendError::new("TEST", "unused"))
    }

    fn logs(&self, _cursor: Option<u64>, _limit: u16) -> Result<LogPageDto, BackendError> {
        Err(BackendError::new("TEST", "unused"))
    }

    fn enqueue(&self, _command: RemoteCommand) -> Result<(), BackendError> {
        Err(BackendError::new("TEST", "unused"))
    }
}

#[test]
fn legacy_backend_defaults_to_graph_unsupported() {
    let backend = LegacyBackend;
    assert!(!backend.graph_supported());
    assert_eq!(backend.graph_document().unwrap_err().code, "UNSUPPORTED_OPERATION");
    assert_eq!(backend.graph_presets().unwrap_err().code, "UNSUPPORTED_OPERATION");
}
