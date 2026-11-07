export type JsonValue = unknown;

export interface HostProfile {
  id: string;
  label: string;
  host: string;
  port: number;
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
  profile_id: string | null;
  host_label: string | null;
  fingerprint: string | null;
  server_id: string | null;
  last_heartbeat: string | null;
  message: string | null;
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

export interface TerminalDiagnostics {
  lines: string[];
  detailed_ice_enabled: boolean;
}

export type UiEvent =
  | { kind: "connection"; data: ConnectionState }
  | { kind: "pairing_invitation"; data: HostProfile }
  | { kind: "snapshot"; data: HostSnapshot }
  | { kind: "capabilities"; data: Capabilities }
  | { kind: "catalog"; data: HostCatalog }
  | { kind: "logs"; data: HostLogs }
  | { kind: "error"; data: string };
