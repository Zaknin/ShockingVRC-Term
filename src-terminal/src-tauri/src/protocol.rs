use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthDto {
    pub protocol_min: u8,
    pub protocol_max: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesDto {
    pub operations: Vec<String>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDto {
    pub bridge_revision: u64,
    pub active_device: usize,
    pub active_device_mac: Option<String>,
    pub devices: Vec<DeviceDto>,
    pub vrchat: VrchatDto,
    pub avatar_zones: Vec<AvatarZoneDto>,
    pub config: ConfigDto,
    pub status: Value,
    pub alarm: Value,
    #[serde(default)]
    pub prefs: Option<HostPrefsDto>,
    pub remote_policy: RemotePolicyDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceDto {
    pub mac: String,
    pub name: String,
    pub battery: Option<u8>,
    pub connected: bool,
    pub reconnecting: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VrchatDto {
    pub connected: bool,
    pub osc_port: u16,
    pub oscquery_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarZoneDto {
    pub zone_type: String,
    pub id: String,
    pub is_tps: bool,
    pub level: f32,
    pub velocity: f32,
    pub acceleration: f32,
    pub recoil: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDto {
    pub channel_a: ChannelConfigDto,
    pub channel_b: ChannelConfigDto,
    pub ukf: Value,
    pub norms: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfigDto {
    pub zones: Vec<ZoneDto>,
    pub frequency: [u8; 4],
    pub intensity: [u8; 4],
    pub limits: LimitsDto,
    pub aggregation: String,
    pub freq_modulation: [Option<Value>; 4],
    pub intensity_modulation: [Option<Value>; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneDto {
    pub zone_type: String,
    pub name: String,
    pub mode: String,
    pub scale: u8,
    pub min_threshold: u8,
    pub max_threshold: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsDto {
    pub min: u8,
    pub max: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostPrefsDto {
    pub auto_save: bool,
    pub alarm_tab_visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemotePolicyDto {
    pub remote_strength: RemoteStrengthDto,
    pub max_remote_sessions: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteStrengthDto {
    pub enabled: bool,
    pub max_by_channel: RemoteChannelCapsDto,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteChannelCapsDto {
    #[serde(rename = "A")]
    pub a: Option<u8>,
    #[serde(rename = "B")]
    pub b: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogDto {
    pub catalog: String,
    pub entries: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsDto {
    pub cursor: String,
    pub next_cursor: Option<String>,
    pub lines: Vec<LogLineDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalDiagnosticsDto {
    pub lines: Vec<String>,
    pub detailed_ice_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLineDto {
    pub seq: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum UiEvent {
    Connection(ConnectionStateDto),
    Snapshot(SnapshotDto),
    Capabilities(CapabilitiesDto),
    Catalog(CatalogDto),
    Logs(LogsDto),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStateDto {
    pub phase: String,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub ice_candidate_pair: Option<String>,
    pub profile_id: Option<String>,
    pub host_label: Option<String>,
    pub fingerprint: Option<String>,
    pub server_id: Option<String>,
    pub last_heartbeat: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDto {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    #[serde(default)]
    pub server_id: String,
    #[serde(default = "default_connection_mode")]
    pub connection_mode: String,
    #[serde(default)]
    pub rendezvous_url: String,
    #[serde(default)]
    pub ice_server_urls: Vec<String>,
    #[serde(default)]
    pub force_relay: bool,
}

fn default_connection_mode() -> String { "auto".to_owned() }

impl ProfileDto {
    pub fn http_base_url(&self) -> String {
        format!("https://{}:{}/", self.url_host(), self.port)
    }

    pub fn wss_base_url(&self) -> String {
        format!("wss://{}:{}/", self.url_host(), self.port)
    }

    fn url_host(&self) -> String {
        if self.host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileInputDto {
    pub id: Option<String>,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    #[serde(default)]
    pub server_id: String,
    #[serde(default = "default_connection_mode")]
    pub connection_mode: String,
    #[serde(default)]
    pub rendezvous_url: String,
    #[serde(default)]
    pub ice_server_urls: Vec<String>,
    #[serde(default)]
    pub force_relay: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostIdentityDto {
    pub profile_id: String,
    pub host_label: String,
    pub server_id: String,
    pub fingerprint: String,
    pub protocol: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandAckDto {
    pub request_id: String,
    pub queued: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ConnectionStateDto, ProfileDto};

    #[test]
    fn profile_keeps_force_relay_policy_when_loaded_and_saved() {
        let profile: ProfileDto = serde_json::from_value(json!({
            "id": "profile-id",
            "label": "Test",
            "host": "127.0.0.1",
            "port": 4318,
            "fingerprint": "a".repeat(64),
            "server_id": "host-id",
            "connection_mode": "traversal_only",
            "rendezvous_url": "wss://signal.example/api/signal/v1",
            "ice_server_urls": [],
            "force_relay": true
        }))
        .expect("profile JSON should deserialize");

        let saved = serde_json::to_value(profile).expect("profile should serialize");
        assert_eq!(saved["force_relay"], true);
    }

    #[test]
    fn older_profiles_default_to_normal_ice_candidates() {
        let profile: ProfileDto = serde_json::from_value(json!({
            "id": "profile-id",
            "label": "Test",
            "host": "127.0.0.1",
            "port": 4318,
            "fingerprint": "a".repeat(64),
            "server_id": "host-id",
            "connection_mode": "auto",
            "rendezvous_url": "",
            "ice_server_urls": []
        }))
        .expect("legacy profile should deserialize");

        let saved = serde_json::to_value(profile).expect("profile should serialize");
        assert_eq!(saved["force_relay"], false);
    }

    #[test]
    fn connection_state_keeps_selected_ice_candidate_types() {
        let state: ConnectionStateDto = serde_json::from_value(json!({
            "phase": "online",
            "transport": "webrtc_datachannel",
            "profile_id": "profile-id",
            "host_label": "Test",
            "fingerprint": "a".repeat(64),
            "server_id": "host-id",
            "last_heartbeat": null,
            "message": null,
            "ice_candidate_pair": "LOCAL RELAY ↔ REMOTE HOST"
        }))
        .expect("connection state should deserialize");

        let saved = serde_json::to_value(state).expect("connection state should serialize");
        assert_eq!(saved["ice_candidate_pair"], "LOCAL RELAY ↔ REMOTE HOST");
    }
}
