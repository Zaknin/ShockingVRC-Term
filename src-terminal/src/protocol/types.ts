export type JsonValue = unknown;

export interface HostProfile {
  id: string;
  label: string;
  label_is_custom: boolean;
  host_display_name: string | null;
  host: string | null;
  port: number | null;
  fingerprint: string;
  server_id: string;
  connection_mode: "auto" | "direct_only" | "traversal_only" | string;
  rendezvous_url: string;
  ice_server_urls: string[];
  force_relay: boolean;
}

export interface HostIdentity {
  profile_id: string;
  host_label: string;
  server_id: string;
  fingerprint: string;
  protocol: number;
}

export interface ConnectionState {
  phase: "disconnected" | "connecting" | "identity_verified" | "online" | "reconnecting" | "revoked" | string;
  transport: string | null;
  ice_candidate_pair: string | null;
  ice_servers_configured: boolean | null;
  rendezvous_stun_received: boolean | null;
  turn_credentials_received: boolean | null;
  local_candidates_gathered: boolean | null;
  profile_id: string | null;
  host_label: string | null;
  fingerprint: string | null;
  server_id: string | null;
  last_heartbeat: string | null;
  message: string | null;
  session_generation?: number | null;
  server_epoch?: string | null;
  transport_generation?: number | null;
}

export interface SnapshotContext {
  seq: number;
  server_epoch: string;
  transport_generation: number;
  session_generation: number;
}

export interface SnapshotEventData extends SnapshotContext {
  state: HostSnapshot;
}

export interface TelemetryEventData extends SnapshotContext {
  avatar_zones: AvatarZone[];
  status: JsonValue;
}

export interface CommandAck {
  request_id: string;
  queued: boolean;
  session_generation: number;
  server_epoch: string;
  transport_generation: number;
  snapshot_seq: number;
}

export interface Device {
  mac: string;
  name: string;
  battery: number | null;
  connected: boolean;
  reconnecting: boolean;
}

export interface AvatarZone {
  zone_type: string;
  id: string;
  is_tps: boolean;
  level: number;
  velocity: number;
  acceleration: number;
  recoil: number;
}

export interface HostZone {
  zone_type: string;
  name: string;
  mode: string;
  scale: number;
  min_threshold: number;
  max_threshold: number;
}

export interface ChannelConfig {
  zones: HostZone[];
  frequency: [number, number, number, number];
  intensity: [number, number, number, number];
  limits: { min: number; max: number };
  aggregation: string;
  freq_modulation: [JsonValue | null, JsonValue | null, JsonValue | null, JsonValue | null];
  intensity_modulation: [JsonValue | null, JsonValue | null, JsonValue | null, JsonValue | null];
}

export interface HostSnapshot {
  bridge_revision: number;
  configuration_state?: "active" | "preview_only" | string | null;
  host_display_name?: string | null;
  active_device: number;
  active_device_mac: string | null;
  devices: Device[];
  vrchat: { connected: boolean; osc_port: number; oscquery_port: number | null };
  avatar_zones: AvatarZone[];
  config: {
    channel_a: ChannelConfig;
    channel_b: ChannelConfig;
    ukf: JsonValue;
    norms: JsonValue;
  };
  status: JsonValue;
  alarm: JsonValue;
  prefs: { auto_save: boolean; alarm_tab_visible: boolean } | null;
  remote_policy: {
    remote_strength: {
      enabled: boolean;
      max_by_channel: { A: number | null; B: number | null };
      scope: string;
    };
    max_remote_sessions: number;
  };
}

export interface Capabilities {
  operations: string[];
  features: string[];
}

export interface HostCatalog {
  catalog: string;
  entries: Record<string, JsonValue>[];
}

export interface HostLogLine {
  seq: number;
  text: string;
}

export interface HostLogs {
  cursor: string;
  next_cursor: string | null;
  lines: HostLogLine[];
}

export interface TerminalDiagnosticEvent {
  timestamp_utc: string;
  process_instance_id: string | null;
  monotonic_ms: number;
  kind: string;
  phase: string | null;
  room_id: string | null;
  request_id: string | null;
  generation: number | null;
  transport: string | null;
  candidate_pair: string | null;
  payload_bytes?: number | null;
  heartbeat_ack_latency_ms?: number | null;
}

export interface HeartbeatIncident {
  id: string;
  started_utc: string;
  completed_utc: string | null;
  outcome: "in_progress" | "recovered" | "session_ended" | "process_restarted_during_recovery";
  room_id: string | null;
  generation: number | null;
  transport: string | null;
  candidate_pair: string | null;
  queue_metrics: {
    action_queue_depth: number;
    action_queue_capacity: number;
    pending_heartbeat_requests: number;
    data_channel_sender_available: boolean | null;
    ice_pair_available: boolean;
    ice_servers_configured: boolean | null;
    rendezvous_stun_received: boolean | null;
    turn_credentials_received: boolean | null;
    local_candidates_gathered: boolean | null;
  };
  prelude_events: TerminalDiagnosticEvent[];
  recovery_events: TerminalDiagnosticEvent[];
  recovery_events_dropped: number;
}

export interface ApiTrafficDirectionCounters {
  messages: number;
  payload_bytes: number;
  average_messages_per_sec_milli_since_window_start: number;
  average_payload_bytes_per_sec_milli_since_window_start: number;
}

export interface ApiTrafficCategoryCounters {
  tx: ApiTrafficDirectionCounters;
  rx: ApiTrafficDirectionCounters;
}

export interface ApiTrafficCounters {
  heartbeat: ApiTrafficCategoryCounters;
  state: ApiTrafficCategoryCounters;
  command: ApiTrafficCategoryCounters;
  authentication: ApiTrafficCategoryCounters;
  session: ApiTrafficCategoryCounters;
  other: ApiTrafficCategoryCounters;
}

export interface TerminalDiagnostics {
  lines: string[];
  detailed_ice_enabled: boolean;
  heartbeat_incidents: HeartbeatIncident[];
  runtime: {
    counters: {
      connection_attempts: number;
      connection_established: number;
      connection_failed: number;
      heartbeat_tx: number;
      heartbeat_ack: number;
      heartbeat_timeouts: number;
      snapshot_requests: number;
      snapshot_responses: number;
      full_snapshot_frames: number;
      telemetry_frames: number;
      telemetry_negotiations: number;
      full_snapshot_payload_bytes: number;
      telemetry_payload_bytes: number;
      telemetry_render_samples: number;
      telemetry_render_total_ms: number;
      telemetry_render_max_ms: number;
      resume_attempts: number;
      resume_succeeded: number;
      resume_failed: number;
      signaling_lease_lost: number;
      disconnect_requested: number;
      generation_changes: number;
      api_traffic: ApiTrafficCounters;
    };
    events: TerminalDiagnosticEvent[];
    current_room_id: string | null;
    current_generation: number | null;
    last_heartbeat_age_ms: number | null;
    contact_update_age_ms: number | null;
    observed_ms: number;
    api_traffic_observed_ms: number;
    process_instance_id: string | null;
    monotonic_clock_origin_utc: string | null;
    runtime_coverage_start_utc: string | null;
    runtime_coverage_end_utc: string | null;
    retained_event_count: number;
    exported_event_count: number;
    events_overwritten: number;
    events_dropped: number;
    events_excluded_by_time_range: number;
    events_unknown_timestamp: number;
    counters_scope: string | null;
  };
}

export interface DiagnosticExportRequest {
  source: "terminal" | "host" | "both";
  timeRange: "all" | "last_1_hour" | "last_6_hours" | "last_24_hours" | "custom";
  terminalTimezone: string;
  startLocal?: string;
  endLocal?: string;
  startUtc?: string;
  endUtc?: string;
  openFolderAfterExport: boolean;
}

export interface DiagnosticExportResult {
  outputPath: string;
  explorerOpened: boolean;
  exportFileSelected: boolean;
  explorerError: string | null;
}

export interface DiagnosticExportProgress {
  running: boolean;
  stage: string;
  progress_percent: number;
}

export type UiEvent =
  | { kind: "connection"; data: ConnectionState }
  | { kind: "pairing_invitation"; data: HostProfile }
  | { kind: "snapshot"; data: SnapshotEventData }
  | { kind: "telemetry"; data: TelemetryEventData }
  | { kind: "capabilities"; data: Capabilities }
  | { kind: "catalog"; data: HostCatalog }
  | { kind: "error"; data: string };
