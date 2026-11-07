use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SETTINGS_FILE: &str = "remote_access.json";
pub const DEFAULT_LISTEN_PORT: u16 = 4318;
pub const MAX_POWER_LIMIT: u8 = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteAccessSettings {
    /// Desired Host listener state. Defaults to OFF.
    pub remote_enabled: bool,
    pub listen_port: u16,
    pub remote_strength: RemoteStrengthPolicy,
}

impl Default for RemoteAccessSettings {
    fn default() -> Self {
        Self {
            remote_enabled: false,
            listen_port: DEFAULT_LISTEN_PORT,
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

        let settings: Self = serde_json::from_str(&raw)?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self) -> Result<(), SettingsError> {
        self.save_to(default_settings_path())
    }

    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        self.validate()?;
        let raw = serde_json::to_string_pretty(self)?;
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
        self.remote_strength.validate()
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
