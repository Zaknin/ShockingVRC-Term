use std::{net::{IpAddr, Ipv4Addr}, path::{Path, PathBuf}};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SETTINGS_FILE: &str = "remote_access.json";
pub const DEFAULT_LISTEN_PORT: u16 = 4318;
pub const MAX_POWER_LIMIT: u8 = 200;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum InvitationMode {
    Direct,
    Internet,
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteAccessSettings {
    /// Desired Host listener state. Defaults to OFF.
    pub remote_enabled: bool,
    pub listen_port: u16,
    /// Optional self-hosted rendezvous registration for ICE traversal.
    pub traversal_enabled: bool,
    /// Route selection for newly created signed invitations.
    pub invitation_mode: InvitationMode,
    pub rendezvous_url: String,
    /// Optional public Host address embedded in direct-only or hybrid invitations.
    pub direct_host: String,
    /// Optional display label sent to paired Terminals in authenticated snapshots.
    pub host_display_name: String,
    /// Explicit STUN/TURN URIs; blank defaults mean no discovery services are contacted.
    pub ice_server_urls: Vec<String>,
    pub remote_strength: RemoteStrengthPolicy,
}

impl Default for RemoteAccessSettings {
    fn default() -> Self {
        Self {
            remote_enabled: false,
            listen_port: DEFAULT_LISTEN_PORT,
            traversal_enabled: false,
            invitation_mode: InvitationMode::Hybrid,
            rendezvous_url: String::new(),
            direct_host: String::new(),
            host_display_name: String::new(),
            ice_server_urls: Vec::new(),
            remote_strength: RemoteStrengthPolicy::default(),
        }
    }
}

impl RemoteAccessSettings {
    pub fn load() -> Result<Self, SettingsError> {
        Self::load_from(default_settings_path())
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(SettingsError::Io(error)),
        };

        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let has_invitation_mode = value.get("invitation_mode").is_some();
        let mut settings: Self = serde_json::from_value(value)?;
        settings.ice_server_urls = shocking_vrc_rendezvous::protocol::normalize_ice_server_uris(&settings.ice_server_urls)
            .map_err(|_| SettingsError::InvalidIceServerUrl)?;
        if !has_invitation_mode {
            settings.invitation_mode = settings.legacy_invitation_mode();
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self) -> Result<(), SettingsError> {
        self.save_to(default_settings_path())
    }

    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        let mut normalized = self.clone();
        normalized.ice_server_urls = shocking_vrc_rendezvous::protocol::normalize_ice_server_uris(&normalized.ice_server_urls)
            .map_err(|_| SettingsError::InvalidIceServerUrl)?;
        normalized.validate()?;
        let raw = serde_json::to_string_pretty(&normalized)?;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, raw)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.listen_port == 0 {
            return Err(SettingsError::InvalidListenPort);
        }
        if self.traversal_enabled {
            if self.rendezvous_url.trim().is_empty() {
                return Err(SettingsError::RendezvousUrlRequired);
            }
            shocking_vrc_rtc::validate_signal_url(self.rendezvous_url.trim())
                .map_err(|_| SettingsError::InvalidRendezvousUrl)?;
        }
        if self.invitation_mode != InvitationMode::Internet
            && !self.direct_host.trim().is_empty()
            && !is_valid_direct_host(&self.direct_host)
        {
            return Err(SettingsError::InvalidDirectHost);
        }
        if self.host_display_name.trim().chars().count() > 80 {
            return Err(SettingsError::InvalidHostDisplayName);
        }
        shocking_vrc_rendezvous::protocol::normalize_ice_server_uris(&self.ice_server_urls)
            .map_err(|_| SettingsError::InvalidIceServerUrl)?;
        self.remote_strength.validate()
    }

    fn legacy_invitation_mode(&self) -> InvitationMode {
        let direct = is_valid_direct_host(&self.direct_host);
        let traversal = self.traversal_enabled && !self.rendezvous_url.trim().is_empty();
        match (direct, traversal) {
            (true, true) => InvitationMode::Hybrid,
            (true, false) => InvitationMode::Direct,
            (false, true) => InvitationMode::Internet,
            (false, false) => InvitationMode::Hybrid,
        }
    }
}

pub fn is_valid_direct_host(value: &str) -> bool {
    let host = value.trim();
    if host.is_empty()
        || host.len() > 255
        || host.chars().any(char::is_whitespace)
        || host.contains(['/', '@', '#', '?', '[', ']', ':'])
    {
        return false;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            address != Ipv4Addr::BROADCAST && !address.is_unspecified() && !address.is_multicast()
        }
        Ok(IpAddr::V6(_)) => false,
        Err(_) => true,
    }
}

fn default_settings_path() -> PathBuf {
    #[cfg(windows)]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        return PathBuf::from(app_data).join("ShockingVRC").join(SETTINGS_FILE);
    }
    PathBuf::from(SETTINGS_FILE)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteStrengthPolicy {
    pub enabled: bool,
    pub max_by_channel: ChannelCaps,
}

impl Default for RemoteStrengthPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_by_channel: ChannelCaps::default(),
        }
    }
}

impl RemoteStrengthPolicy {
    /// Enables the remote-only cap policy without imposing a new cap on saved settings.
    /// Missing channel values start at the existing unrestricted ceiling of 200.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            self.max_by_channel.a.get_or_insert(MAX_POWER_LIMIT);
            self.max_by_channel.b.get_or_insert(MAX_POWER_LIMIT);
        }
        self.enabled = enabled;
    }

    fn validate(&self) -> Result<(), SettingsError> {
        for (channel, value) in [
            (RemoteChannel::A, self.max_by_channel.a),
            (RemoteChannel::B, self.max_by_channel.b),
        ] {
            if value.is_some_and(|cap| cap > MAX_POWER_LIMIT) {
                return Err(SettingsError::InvalidCap { channel });
            }
            if self.enabled && value.is_none() {
                return Err(SettingsError::MissingCap { channel });
            }
        }
        Ok(())
    }

    pub fn cap_for(&self, channel: RemoteChannel) -> Option<u8> {
        match channel {
            RemoteChannel::A => self.max_by_channel.a,
            RemoteChannel::B => self.max_by_channel.b,
        }
    }

    /// Validates only remote Power Limits writes. It deliberately says
    /// nothing about local GUI edits or physical output strength.
    pub fn validate_limit_write(
        &self,
        operation: RemoteLimitOperation,
        channel: RemoteChannel,
        value: u8,
        current_min: u8,
    ) -> Result<(), StrengthPolicyError> {
        if value > MAX_POWER_LIMIT {
            return Err(StrengthPolicyError::InvalidValue { value });
        }
        if !self.enabled {
            return Ok(());
        }

        let cap = self
            .cap_for(channel)
            .ok_or(StrengthPolicyError::MissingCap { channel })?;
        if value > cap {
            return Err(StrengthPolicyError::LimitExceeded {
                channel,
                requested: value,
                cap,
            });
        }

        if operation == RemoteLimitOperation::Max && current_min > value && current_min > cap {
            return Err(StrengthPolicyError::MaxWouldNormalizeAboveCap {
                channel,
                current_min,
                requested_max: value,
                cap,
            });
        }

        Ok(())
    }

}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelCaps {
    #[serde(rename = "A")]
    pub a: Option<u8>,
    #[serde(rename = "B")]
    pub b: Option<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RemoteChannel {
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteLimitOperation {
    Min,
    Max,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("remote listener port must be between 1 and 65535")]
    InvalidListenPort,
    #[error("enable a rendezvous URL before enabling traversal")]
    RendezvousUrlRequired,
    #[error("rendezvous URL must use wss:// (or ws:// on loopback)")]
    InvalidRendezvousUrl,
    #[error("ICE server URLs must use valid stun:, stuns:, turn:, or turns: URIs without embedded credentials")]
    InvalidIceServerUrl,
    #[error("public direct Host address must be a hostname or IPv4 address without a scheme or port")]
    InvalidDirectHost,
    #[error("Host display name must be 80 characters or fewer")]
    InvalidHostDisplayName,
    #[error("remote strength cap for channel {channel:?} is missing")]
    MissingCap { channel: RemoteChannel },
    #[error("remote strength cap for channel {channel:?} must be between 0 and 200")]
    InvalidCap { channel: RemoteChannel },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StrengthPolicyError {
    #[error("requested power limit {value} is outside 0..=200")]
    InvalidValue { value: u8 },
    #[error("remote strength cap for channel {channel:?} is not configured")]
    MissingCap { channel: RemoteChannel },
    #[error("requested {channel:?} power limit {requested} exceeds the remote cap {cap}")]
    LimitExceeded {
        channel: RemoteChannel,
        requested: u8,
        cap: u8,
    },
    #[error(
        "requested {channel:?} maximum {requested_max} would normalize to the current minimum {current_min}, above cap {cap}"
    )]
    MaxWouldNormalizeAboveCap {
        channel: RemoteChannel,
        current_min: u8,
        requested_max: u8,
        cap: u8,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_display_name_is_optional_and_limited_to_eighty_characters() {
        let mut settings = RemoteAccessSettings::default();
        assert!(settings.host_display_name.is_empty());
        assert!(settings.validate().is_ok());
        settings.host_display_name = " Nightmare's Coyote ".to_owned();
        assert!(settings.validate().is_ok());
        settings.host_display_name = "x".repeat(81);
        assert!(matches!(settings.validate(), Err(SettingsError::InvalidHostDisplayName)));
    }

    #[test]
    fn enabling_missing_remote_caps_initializes_both_to_two_hundred() {
        let mut policy = RemoteStrengthPolicy::default();
        policy.set_enabled(true);
        assert!(policy.enabled);
        assert_eq!(policy.max_by_channel.a, Some(200));
        assert_eq!(policy.max_by_channel.b, Some(200));
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn enabling_remote_caps_preserves_configured_value_and_fills_only_missing_value() {
        let mut policy = RemoteStrengthPolicy::default();
        policy.max_by_channel.a = Some(137);
        policy.set_enabled(true);
        assert_eq!(policy.max_by_channel.a, Some(137));
        assert_eq!(policy.max_by_channel.b, Some(200));
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn disabling_and_reenabling_remote_caps_preserves_saved_values() {
        let mut policy = RemoteStrengthPolicy::default();
        policy.max_by_channel.a = Some(137);
        policy.set_enabled(true);
        policy.max_by_channel.b = Some(155);
        policy.set_enabled(false);
        assert!(!policy.enabled);
        assert_eq!(policy.max_by_channel.a, Some(137));
        assert_eq!(policy.max_by_channel.b, Some(155));
        policy.set_enabled(true);
        assert!(policy.enabled);
        assert_eq!(policy.max_by_channel.a, Some(137));
        assert_eq!(policy.max_by_channel.b, Some(155));
    }

    #[test]
    fn direct_invitation_accepts_lan_ipv4_but_rejects_wildcard_and_ipv6_literals() {
        let mut settings = RemoteAccessSettings::default();
        settings.direct_host = "192.168.1.25".to_owned();
        assert!(settings.validate().is_ok());
        settings.direct_host = "0.0.0.0".to_owned();
        assert!(matches!(settings.validate(), Err(SettingsError::InvalidDirectHost)));
        settings.direct_host = "::1".to_owned();
        assert!(matches!(settings.validate(), Err(SettingsError::InvalidDirectHost)));
    }

    #[test]
    fn invitation_readiness_requires_a_connectable_direct_address() {
        assert!(is_valid_direct_host("192.168.1.25"));
        assert!(is_valid_direct_host("host.example.com"));
        assert!(is_valid_direct_host("127.0.0.1"));
        assert!(!is_valid_direct_host("host.example.com:443"));
        assert!(!is_valid_direct_host(""));
        assert!(!is_valid_direct_host("0.0.0.0"));
        assert!(!is_valid_direct_host("255.255.255.255"));
        assert!(!is_valid_direct_host("::1"));
    }

    #[test]
    fn settings_accept_an_explicit_internet_invitation_mode_without_a_direct_host() {
        let path = std::env::temp_dir().join(format!("shockingvrc-settings-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, serde_json::json!({
            "remote_enabled": true,
            "listen_port": 4318,
            "traversal_enabled": true,
            "rendezvous_url": "wss://rendezvous.example.test/api/signal/v1",
            "direct_host": "",
            "invitation_mode": "internet"
        }).to_string()).unwrap();

        let settings = RemoteAccessSettings::load_from(&path).expect("explicit Internet mode should load");
        let serialized = serde_json::to_value(settings).unwrap();

        assert_eq!(serialized["invitation_mode"], "internet");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_direct_only_settings_migrate_to_direct_invitation_mode() {
        let path = std::env::temp_dir().join(format!("shockingvrc-settings-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, serde_json::json!({
            "remote_enabled": true,
            "listen_port": 4318,
            "traversal_enabled": false,
            "rendezvous_url": "",
            "direct_host": "192.168.1.25"
        }).to_string()).unwrap();

        let settings = RemoteAccessSettings::load_from(&path).expect("legacy settings should migrate");
        let serialized = serde_json::to_value(settings).unwrap();

        assert_eq!(serialized["invitation_mode"], "direct");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn custom_ice_settings_canonicalize_dedupe_ipv6_and_reject_uri_credentials() {
        let path = std::env::temp_dir().join(format!("shockingvrc-settings-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, serde_json::json!({
            "ice_server_urls": [
                "STUN:Example.test",
                "stun:example.test:3478",
                "turn:[2001:db8::1]?transport=udp"
            ]
        }).to_string()).unwrap();
        let settings = RemoteAccessSettings::load_from(&path).expect("canonical custom ICE URI list");
        assert_eq!(settings.ice_server_urls, [
            "stun:example.test:3478",
            "turn:[2001:db8::1]:3478?transport=udp",
        ]);
        let _ = std::fs::remove_file(&path);

        let mut settings = RemoteAccessSettings::default();
        settings.ice_server_urls = vec!["turn:user:password@relay.example.test".to_owned()];
        assert!(matches!(settings.validate(), Err(SettingsError::InvalidIceServerUrl)));
    }
}
