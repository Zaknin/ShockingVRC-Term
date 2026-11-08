/// The approved Remote API v1 command names from `docs/06-command-matrix.md`.
/// Updater commands are intentionally absent.
pub const REMOTE_OPERATIONS: [&str; 37] = [
    "devices.switch",
    "zones.add",
    "zones.add_all_from_avatar",
    "zones.remove",
    "zones.set_mode",
    "zones.set_scale",
    "zones.set_threshold_min",
    "zones.set_threshold_max",
    "channels.set_frequency",
    "channels.set_intensity",
    "channels.set_limit_min",
    "channels.set_limit_max",
    "channels.set_aggregation",
    "config.save",
    "config.load",
    "tuning.set_ukf",
    "tuning.reset_ukf",
    "tuning.set_norms",
    "tuning.reset_norms",
    "modulation.apply",
    "modulation.clear_segment",
    "modulation.clear_channel",
    "presets.refresh",
    "presets.apply",
    "presets.save_user",
    "presets.delete_user",
    "zone_presets.reload",
    "zone_presets.apply",
    "zone_presets.save",
    "zone_presets.delete",
    "alarm.set_config",
    "alarm.set_channels",
    "alarm.test",
    "alarm.stop",
    "alarm.snooze",
    "prefs.set_auto_save",
    "prefs.set_alarm_tab_visible",
];

pub const REMOTE_GRAPH_OPERATIONS: [&str; 1] = ["engine.set_mode"];

/// Returns the operation catalog to advertise for a backend's negotiated capabilities.
///
/// Graph mode switching is accepted by the protocol parser for validation and dispatch,
/// but legacy Hosts must not advertise it as an available operation.
pub fn remote_operations_for(graph_supported: bool) -> Vec<&'static str> {
    let mut operations = REMOTE_OPERATIONS.to_vec();
    if graph_supported {
        operations.extend(REMOTE_GRAPH_OPERATIONS);
    }
    operations
}

pub fn is_remote_operation(operation: &str) -> bool {
    REMOTE_OPERATIONS.contains(&operation) || REMOTE_GRAPH_OPERATIONS.contains(&operation)
}
