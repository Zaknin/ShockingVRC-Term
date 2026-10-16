use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use subtle::ConstantTimeEq;
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct LoadedConfig {
    pub server: ServerConfig,
    pub rooms: RoomConfig,
    pub turn: TurnConfig,
    pub hosts: HashMap<String, HostAuthorization>,
    pub turn_secret: Option<Vec<u8>>,
    pub config_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct HostAuthorization {
    pub registration_key: String,
    pub invite_token: String,
}

impl HostAuthorization {
    pub fn registration_matches(&self, candidate: &str) -> bool {
        constant_time_eq(self.registration_key.as_bytes(), candidate.as_bytes())
    }

    pub fn invite_matches(&self, candidate: &str) -> bool {
        constant_time_eq(self.invite_token.as_bytes(), candidate.as_bytes())
    }
}

fn constant_time_eq(expected: &[u8], candidate: &[u8]) -> bool {
    expected.len() == candidate.len() && bool::from(expected.ct_eq(candidate))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default)]
    pub public_url: String,
    #[serde(default = "default_health_path")]
    pub health_path: String,
    #[serde(default = "default_signal_path")]
    pub signal_path: String,
    #[serde(default = "default_version_path")]
    pub version_path: String,
    #[serde(default)]
    pub tls: TlsConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    #[serde(default)]
    pub cert_file: String,
    #[serde(default)]
    pub key_file: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomConfig {
    #[serde(default = "default_room_ttl")]
    pub ttl_seconds: u64,
    #[serde(default = "default_candidate_limit")]
    pub max_candidates_per_peer: usize,
    #[serde(default = "default_frame_limit")]
    pub max_signal_frame_bytes: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub uris: Vec<String>,
    #[serde(default)]
    pub realm: String,
    #[serde(default)]
    pub secret_file: String,
    #[serde(default = "default_turn_ttl")]
    pub credential_ttl_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    server: ServerConfig,
    #[serde(default)]
    rooms: RoomConfig,
    #[serde(default)]
    turn: TurnConfig,
    registration: RegistrationConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrationConfig {
    #[serde(default)]
    policy_file: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(default)]
    hosts: Vec<PolicyHost>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyHost {
    host_id: String,
    registration_key: String,
    invite_token: String,
}

impl LoadedConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|error| ConfigError::ReadConfig(error.to_string()))?;
        let raw: RawConfig = toml::from_str(&source).map_err(|error| ConfigError::Parse(error.to_string()))?;
        let config_dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();

        let bind: SocketAddr = raw
            .server
            .bind
            .parse()
            .map_err(|_| ConfigError::Invalid("server.bind must be a socket address"))?;
        let _ = bind;
        validate_path("server.health_path", &raw.server.health_path)?;
        validate_path("server.signal_path", &raw.server.signal_path)?;
        validate_path("server.version_path", &raw.server.version_path)?;
        if !raw.server.public_url.is_empty() && !raw.server.public_url.starts_with("https://") {
            return Err(ConfigError::Invalid("server.public_url must be blank or use https://"));
        }
        if is_placeholder(&raw.server.public_url) {
            return Err(ConfigError::Invalid("server.public_url contains a template placeholder"));
        }
        let cert = raw.server.tls.cert_file.is_empty();
        let key = raw.server.tls.key_file.is_empty();
        if cert != key {
            return Err(ConfigError::Invalid("server.tls.cert_file and key_file must be set together"));
        }
        if is_placeholder(&raw.server.tls.cert_file) || is_placeholder(&raw.server.tls.key_file) {
            return Err(ConfigError::Invalid("server TLS paths contain a template placeholder"));
        }
        if raw.rooms.ttl_seconds == 0 {
            return Err(ConfigError::Invalid("rooms.ttl_seconds must be greater than zero"));
        }
        if raw.rooms.max_candidates_per_peer == 0 {
            return Err(ConfigError::Invalid("rooms.max_candidates_per_peer must be greater than zero"));
        }
        if !(1..=65_536).contains(&raw.rooms.max_signal_frame_bytes) {
            return Err(ConfigError::Invalid("rooms.max_signal_frame_bytes must be between 1 and 65536"));
        }
        if raw.turn.credential_ttl_seconds == 0 {
            return Err(ConfigError::Invalid("turn.credential_ttl_seconds must be greater than zero"));
        }
        let hosts = if raw.registration.policy_file.is_empty() {
            return Err(ConfigError::Invalid("registration.policy_file is required"));
        } else {
            let policy_path = resolve_path(&config_dir, &raw.registration.policy_file);
            let policy_text =
                fs::read_to_string(policy_path).map_err(|error| ConfigError::ReadPolicy(error.to_string()))?;
            let policy: PolicyFile =
                toml::from_str(&policy_text).map_err(|error| ConfigError::ParsePolicy(error.to_string()))?;
            let mut hosts = HashMap::new();
            for entry in policy.hosts {
                if entry.host_id.is_empty() || entry.host_id.len() > 256 {
                    return Err(ConfigError::Invalid("policy host_id must contain 1 to 256 bytes"));
                }
                if entry.registration_key.is_empty() || entry.registration_key.len() > 4096 {
                    return Err(ConfigError::Invalid("policy registration_key must contain 1 to 4096 bytes"));
                }
                if entry.invite_token.is_empty() || entry.invite_token.len() > 4096 {
                    return Err(ConfigError::Invalid("policy invite_token must contain 1 to 4096 bytes"));
                }
                if is_placeholder(&entry.host_id)
                    || is_placeholder(&entry.registration_key)
                    || is_placeholder(&entry.invite_token)
                {
                    return Err(ConfigError::Invalid("registration policy contains a template placeholder"));
                }
                if entry.registration_key == entry.invite_token {
                    return Err(ConfigError::Invalid("registration_key and invite_token must be distinct"));
                }
                if hosts
                    .insert(
                        entry.host_id,
                        HostAuthorization {
                            registration_key: entry.registration_key,
                            invite_token: entry.invite_token,
                        },
                    )
                    .is_some()
                {
                    return Err(ConfigError::Invalid("registration policy contains a duplicate host_id"));
                }
            }
            hosts
        };

        let turn_secret = if raw.turn.enabled {
            if raw.turn.uris.is_empty()
                || raw.turn.uris.len() > 16
                || raw.turn.realm.trim().is_empty()
                || raw.turn.secret_file.is_empty()
            {
                return Err(ConfigError::Invalid(
                    "enabled TURN requires 1 to 16 URIs, a realm, and secret_file",
                ));
            }
            if is_placeholder(&raw.turn.realm)
                || raw.turn.uris.iter().any(|uri| is_placeholder(uri))
                || is_placeholder(&raw.turn.secret_file)
            {
                return Err(ConfigError::Invalid("TURN configuration contains a template placeholder"));
            }
            let secret_path = resolve_path(&config_dir, &raw.turn.secret_file);
            let secret = fs::read(secret_path).map_err(|error| ConfigError::ReadTurnSecret(error.to_string()))?;
            let secret = trim_ascii_line_endings(secret);
            if secret.is_empty() {
                return Err(ConfigError::Invalid("TURN secret file is empty"));
            }
            if secret.windows(b"REPLACE_".len()).any(|value| value == b"REPLACE_") {
                return Err(ConfigError::Invalid("TURN secret file contains a template placeholder"));
            }
            Some(secret)
        } else {
            None
        };

        Ok(Self {
            server: raw.server,
            rooms: raw.rooms,
            turn: raw.turn,
            hosts,
            turn_secret,
            config_dir,
        })
    }
}

fn resolve_path(base: &Path, configured: &str) -> PathBuf {
    let path = PathBuf::from(configured);
    if path.is_absolute() { path } else { base.join(path) }
}

fn validate_path(name: &'static str, path: &str) -> Result<(), ConfigError> {
    if !path.starts_with('/') || path.contains('?') || path.contains('#') || path.contains("..") {
        return Err(ConfigError::Invalid(name));
    }
    Ok(())
}

fn is_placeholder(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("replace_") || value.contains("example.") || value.contains(".invalid") || value.starts_with('<')
}

fn trim_ascii_line_endings(mut bytes: Vec<u8>) -> Vec<u8> {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    bytes
}

fn default_bind() -> String {
    "127.0.0.1:8087".into()
}
fn default_health_path() -> String {
    "/healthz".into()
}
fn default_signal_path() -> String {
    "/api/signal/v1".into()
}
fn default_version_path() -> String {
    "/version".into()
}
fn default_room_ttl() -> u64 {
    60
}
fn default_candidate_limit() -> usize {
    64
}
fn default_frame_limit() -> usize {
    65_536
}
fn default_turn_ttl() -> u64 {
    600
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            ttl_seconds: default_room_ttl(),
            max_candidates_per_peer: default_candidate_limit(),
            max_signal_frame_bytes: default_frame_limit(),
        }
    }
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            uris: Vec::new(),
            realm: String::new(),
            secret_file: String::new(),
            credential_ttl_seconds: default_turn_ttl(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read rendezvous config: {0}")]
    ReadConfig(String),
    #[error("invalid rendezvous config: {0}")]
    Parse(String),
    #[error("cannot read registration policy: {0}")]
    ReadPolicy(String),
    #[error("invalid registration policy: {0}")]
    ParsePolicy(String),
    #[error("cannot read TURN secret file: {0}")]
    ReadTurnSecret(String),
    #[error("configuration validation failed: {0}")]
    Invalid(&'static str),
}
