use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const SIGNAL_PROTOCOL: u8 = 1;
/// Optional extension used only for TURN credential discovery. Clients retry with v1 when the
/// Rendezvous does not support this extension, keeping every existing v1 message unchanged.
pub const ICE_DISCOVERY_PROTOCOL: u8 = 2;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum SignalMessage {
    #[serde(rename = "register")]
    Register {
        protocol: u8,
        request_id: Uuid,
        host_id: String,
        identity_public_key: String,
        authorization: String,
    },
    #[serde(rename = "registered")]
    Registered {
        protocol: u8,
        request_id: Uuid,
        host_id: String,
        registration_epoch: Uuid,
    },
    #[serde(rename = "join")]
    Join {
        protocol: u8,
        request_id: Uuid,
        host_id: String,
        authorization: String,
        #[serde(default, skip_serializing_if = "is_false")]
        force_relay: bool,
    },
    #[serde(rename = "join.accepted")]
    JoinAccepted {
        protocol: u8,
        request_id: Uuid,
        host_id: String,
        room_id: String,
        nonce: String,
        #[serde(default, skip_serializing_if = "is_false")]
        force_relay: bool,
    },
    #[serde(rename = "signal.offer")]
    SignalOffer {
        protocol: u8,
        request_id: Uuid,
        room_id: String,
        sdp: String,
    },
    #[serde(rename = "signal.answer")]
    SignalAnswer {
        protocol: u8,
        request_id: Uuid,
        room_id: String,
        sdp: String,
    },
    #[serde(rename = "signal.candidate")]
    SignalCandidate {
        protocol: u8,
        request_id: Uuid,
        room_id: String,
        candidate: String,
    },
    #[serde(rename = "signal.end_of_candidates")]
    SignalEndOfCandidates {
        protocol: u8,
        request_id: Uuid,
        room_id: String,
    },
    #[serde(rename = "turn.credentials.request")]
    TurnCredentialsRequest {
        protocol: u8,
        request_id: Uuid,
        room_id: String,
    },
    #[serde(rename = "turn.credentials.result")]
    TurnCredentialsResult {
        protocol: u8,
        request_id: Uuid,
        room_id: String,
        turn_uris: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stun_uris: Vec<String>,
        username: String,
        credential: String,
        expires_at: u64,
    },
    #[serde(rename = "room.close")]
    RoomClose {
        protocol: u8,
        request_id: Uuid,
        room_id: String,
    },
    #[serde(rename = "error")]
    Error {
        protocol: u8,
        request_id: Uuid,
        code: String,
        message: String,
    },
}

fn is_false(value: &bool) -> bool {
    !value
}

impl SignalMessage {
    pub fn request_id(&self) -> Uuid {
        match self {
            Self::Register { request_id, .. }
            | Self::Registered { request_id, .. }
            | Self::Join { request_id, .. }
            | Self::JoinAccepted { request_id, .. }
            | Self::SignalOffer { request_id, .. }
            | Self::SignalAnswer { request_id, .. }
            | Self::SignalCandidate { request_id, .. }
            | Self::SignalEndOfCandidates { request_id, .. }
            | Self::TurnCredentialsRequest { request_id, .. }
            | Self::TurnCredentialsResult { request_id, .. }
            | Self::RoomClose { request_id, .. }
            | Self::Error { request_id, .. } => *request_id,
        }
    }

    pub fn protocol(&self) -> u8 {
        match self {
            Self::Register { protocol, .. }
            | Self::Registered { protocol, .. }
            | Self::Join { protocol, .. }
            | Self::JoinAccepted { protocol, .. }
            | Self::SignalOffer { protocol, .. }
            | Self::SignalAnswer { protocol, .. }
            | Self::SignalCandidate { protocol, .. }
            | Self::SignalEndOfCandidates { protocol, .. }
            | Self::TurnCredentialsRequest { protocol, .. }
            | Self::TurnCredentialsResult { protocol, .. }
            | Self::RoomClose { protocol, .. }
            | Self::Error { protocol, .. } => *protocol,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        let extended_discovery = matches!(self,
            Self::TurnCredentialsRequest { protocol: ICE_DISCOVERY_PROTOCOL, .. }
                | Self::TurnCredentialsResult { protocol: ICE_DISCOVERY_PROTOCOL, .. }
        );
        if self.protocol() != SIGNAL_PROTOCOL && !extended_discovery {
            return Err(ProtocolError::UnsupportedVersion);
        }
        match self {
            Self::Register {
                host_id,
                identity_public_key,
                authorization,
                ..
            } => {
                check_len("host_id", host_id, 1, 256)?;
                check_len("identity_public_key", identity_public_key, 1, 512)?;
                check_len("authorization", authorization, 1, 4096)?;
            }
            Self::Registered { host_id, .. } | Self::Join { host_id, .. } => {
                check_len("host_id", host_id, 1, 256)?;
                if let Self::Join { authorization, .. } = self {
                    check_len("authorization", authorization, 1, 4096)?;
                }
            }
            Self::JoinAccepted {
                host_id,
                room_id,
                nonce,
                ..
            } => {
                check_len("host_id", host_id, 1, 256)?;
                check_len("room_id", room_id, 1, 256)?;
                check_len("nonce", nonce, 1, 512)?;
            }
            Self::SignalOffer { room_id, sdp, .. } | Self::SignalAnswer { room_id, sdp, .. } => {
                check_len("room_id", room_id, 1, 256)?;
                check_len("sdp", sdp, 0, 64_000)?;
            }
            Self::SignalCandidate { room_id, candidate, .. } => {
                check_len("room_id", room_id, 1, 256)?;
                check_len("candidate", candidate, 0, 8_192)?;
            }
            Self::SignalEndOfCandidates { room_id, .. }
            | Self::TurnCredentialsRequest { room_id, .. }
            | Self::RoomClose { room_id, .. } => check_len("room_id", room_id, 1, 256)?,
            Self::TurnCredentialsResult {
                room_id,
                turn_uris,
                stun_uris,
                protocol,
                username,
                credential,
                ..
            } => {
                check_len("room_id", room_id, 1, 256)?;
                check_len("username", username, 0, 512)?;
                check_len("credential", credential, 0, 512)?;
                if turn_uris.len() > 16 {
                    return Err(ProtocolError::InvalidField("turn_uris"));
                }
                if stun_uris.len() > 16 || stun_uris.iter().any(|uri| !validate_stun_uri(uri)) {
                    return Err(ProtocolError::InvalidField("stun_uris"));
                }
                if *protocol == SIGNAL_PROTOCOL && !stun_uris.is_empty() {
                    return Err(ProtocolError::InvalidField("stun_uris"));
                }
            }
            Self::Error { code, message, .. } => {
                check_len("code", code, 0, 80)?;
                check_len("message", message, 0, 512)?;
            }
        }
        Ok(())
    }

    pub fn error(request_id: Uuid, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            protocol: SIGNAL_PROTOCOL,
            request_id,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn room_close(room_id: impl Into<String>) -> Self {
        Self::RoomClose {
            protocol: SIGNAL_PROTOCOL,
            request_id: Uuid::new_v4(),
            room_id: room_id.into(),
        }
    }
}

/// Validates the supported STUN URI authority syntax, including bracketed IPv6 literals.
pub fn validate_stun_uri(value: &str) -> bool {
    let Some((scheme, authority)) = value.split_once(':') else {
        return false;
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "stun" | "stuns")
        || authority.is_empty()
        || authority.len() > 512
        || authority.chars().any(char::is_whitespace)
        || authority.contains(['/', '@', '#', '?'])
    {
        return false;
    }
    let Ok(parsed) = url::Url::parse(&format!("udp://{authority}")) else {
        return false;
    };
    parsed.host().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && matches!(parsed.path(), "" | "/")
        && parsed.port().is_none_or(|port| port != 0)
}

/// Validates, canonicalizes, and deduplicates recommended STUN URIs.
pub fn normalize_stun_uris(values: &[String]) -> Result<Vec<String>, ProtocolError> {
    let mut normalized = Vec::with_capacity(values.len().min(16));
    for value in values {
        let value = value.trim();
        if !validate_stun_uri(value) {
            return Err(ProtocolError::InvalidField("stun_uris"));
        }
        let (scheme, authority) = value.split_once(':').ok_or(ProtocolError::InvalidField("stun_uris"))?;
        let parsed = url::Url::parse(&format!("udp://{authority}"))
            .map_err(|_| ProtocolError::InvalidField("stun_uris"))?;
        let host = match parsed.host().ok_or(ProtocolError::InvalidField("stun_uris"))? {
            url::Host::Domain(host) => host.to_ascii_lowercase(),
            url::Host::Ipv4(host) => host.to_string(),
            url::Host::Ipv6(host) => format!("[{host}]"),
        };
        let default_port = if scheme.eq_ignore_ascii_case("stuns") { 5349 } else { 3478 };
        let port = parsed.port().unwrap_or(default_port);
        let uri = format!("{}:{host}:{port}", scheme.to_ascii_lowercase());
        if !normalized.iter().any(|existing| existing == &uri) {
            normalized.push(uri);
        }
    }
    if normalized.len() > 16 {
        return Err(ProtocolError::InvalidField("stun_uris"));
    }
    Ok(normalized)
}

/// Canonicalizes a STUN/TURN URI without accepting credentials embedded in the URI.
pub fn normalize_ice_server_uri(value: &str) -> Result<String, ProtocolError> {
    let value = value.trim();
    let (scheme, remainder) = value.split_once(':').ok_or(ProtocolError::InvalidField("ice_server_uri"))?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "stun" | "stuns" | "turn" | "turns") {
        return Err(ProtocolError::InvalidField("ice_server_uri"));
    }
    if scheme == "stun" || scheme == "stuns" {
        return normalize_stun_uris(&[value.to_owned()])
            .map(|uris| uris.into_iter().next().unwrap_or_default());
    }
    if remainder.is_empty() || remainder.len() > 512 || remainder.chars().any(char::is_whitespace) {
        return Err(ProtocolError::InvalidField("ice_server_uri"));
    }
    let (authority, query) = remainder.split_once('?').unwrap_or((remainder, ""));
    if authority.contains(['/', '@', '#', '?']) {
        return Err(ProtocolError::InvalidField("ice_server_uri"));
    }
    let transport = if query.is_empty() {
        None
    } else if scheme == "turn" {
        let Some(transport) = query.strip_prefix("transport=") else {
            return Err(ProtocolError::InvalidField("ice_server_uri"));
        };
        if !matches!(transport.to_ascii_lowercase().as_str(), "udp" | "tcp") {
            return Err(ProtocolError::InvalidField("ice_server_uri"));
        }
        Some(transport.to_ascii_lowercase())
    } else {
        return Err(ProtocolError::InvalidField("ice_server_uri"));
    };
    let parsed = url::Url::parse(&format!("udp://{authority}"))
        .map_err(|_| ProtocolError::InvalidField("ice_server_uri"))?;
    if parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.port().is_some_and(|port| port == 0)
    {
        return Err(ProtocolError::InvalidField("ice_server_uri"));
    }
    let host = match parsed.host().ok_or(ProtocolError::InvalidField("ice_server_uri"))? {
        url::Host::Domain(host) => host.to_ascii_lowercase(),
        url::Host::Ipv4(host) => host.to_string(),
        url::Host::Ipv6(host) => format!("[{host}]"),
    };
    let default_port = if scheme == "turns" { 5349 } else { 3478 };
    let port = parsed.port().unwrap_or(default_port);
    let mut result = format!("{scheme}:{host}:{port}");
    if let Some(transport) = transport {
        result.push_str("?transport=");
        result.push_str(&transport);
    }
    Ok(result)
}

pub fn normalize_ice_server_uris(values: &[String]) -> Result<Vec<String>, ProtocolError> {
    let mut normalized = Vec::with_capacity(values.len().min(16));
    for value in values {
        let uri = normalize_ice_server_uri(value)?;
        if !normalized.contains(&uri) {
            normalized.push(uri);
        }
    }
    if normalized.len() > 16 {
        return Err(ProtocolError::InvalidField("ice_server_uris"));
    }
    Ok(normalized)
}

#[cfg(test)]
mod relay_policy_tests {
    use super::SignalMessage;
    use uuid::Uuid;

    #[test]
    fn relay_only_flag_is_optional_for_existing_protocol_v1_messages() {
        let request_id = Uuid::new_v4();
        let legacy_join = format!(
            r#"{{"type":"join","protocol":1,"request_id":"{request_id}","host_id":"host","authorization":"token"}}"#
        );
        let parsed: SignalMessage = serde_json::from_str(&legacy_join).unwrap();
        assert!(matches!(parsed, SignalMessage::Join { force_relay: false, .. }));

        let accepted = SignalMessage::JoinAccepted {
            protocol: 1,
            request_id,
            host_id: "host".to_owned(),
            room_id: Uuid::new_v4().to_string(),
            nonce: "nonce".to_owned(),
            force_relay: true,
        };
        let json = serde_json::to_value(accepted).unwrap();
        assert_eq!(json["force_relay"], true);
    }
}

fn check_len(field: &'static str, value: &str, min: usize, max: usize) -> Result<(), ProtocolError> {
    if !(min..=max).contains(&value.chars().count()) {
        return Err(ProtocolError::InvalidField(field));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("unsupported_version")]
    UnsupportedVersion,
    #[error("invalid_field:{0}")]
    InvalidField(&'static str),
    #[error("invalid_json")]
    InvalidJson,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_wire_message_variants() {
        let messages = [
            r#"{"protocol":1,"type":"register","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","host_id":"h","identity_public_key":"cHVi","authorization":"key"}"#,
            r#"{"protocol":1,"type":"registered","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","host_id":"h","registration_epoch":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"}"#,
            r#"{"protocol":1,"type":"join","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","host_id":"h","authorization":"invite"}"#,
            r#"{"protocol":1,"type":"join.accepted","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","host_id":"h","room_id":"r","nonce":"n"}"#,
            r#"{"protocol":1,"type":"signal.offer","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r","sdp":"v=0"}"#,
            r#"{"protocol":1,"type":"signal.answer","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r","sdp":"v=0"}"#,
            r#"{"protocol":1,"type":"signal.candidate","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r","candidate":"candidate:x"}"#,
            r#"{"protocol":1,"type":"signal.end_of_candidates","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r"}"#,
            r#"{"protocol":1,"type":"turn.credentials.request","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r"}"#,
            r#"{"protocol":1,"type":"turn.credentials.result","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r","turn_uris":[],"username":"u","credential":"c","expires_at":1}"#,
            r#"{"protocol":1,"type":"room.close","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r"}"#,
            r#"{"protocol":1,"type":"error","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","code":"x","message":"y"}"#,
        ];
        for json in messages {
            let message: SignalMessage = serde_json::from_str(json).unwrap();
            message.validate().unwrap();
        }
    }

    #[test]
    fn rejects_unknown_fields_and_unsupported_protocol() {
        let unknown = r#"{"protocol":1,"type":"room.close","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r","other":1}"#;
        assert!(serde_json::from_str::<SignalMessage>(unknown).is_err());
        let unsupported =
            r#"{"protocol":2,"type":"room.close","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r"}"#;
        assert!(
            serde_json::from_str::<SignalMessage>(unsupported)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn ice_discovery_extension_is_optional_and_limited_to_turn_credentials_messages() {
        let legacy = r#"{"protocol":1,"type":"turn.credentials.result","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r","turn_uris":[],"username":"","credential":"","expires_at":0}"#;
        let legacy: SignalMessage = serde_json::from_str(legacy).unwrap();
        legacy.validate().unwrap();
        assert!(matches!(legacy, SignalMessage::TurnCredentialsResult { .. }));

        let extended = r#"{"protocol":2,"type":"turn.credentials.result","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r","turn_uris":[],"username":"","credential":"","expires_at":0,"stun_uris":["stun:stun.example.test:3478","stun:[2001:db8::1]:3478"]}"#;
        let extended: SignalMessage = serde_json::from_str(extended).unwrap();
        extended.validate().unwrap();
        assert!(matches!(
            extended,
            SignalMessage::TurnCredentialsResult { protocol: 2, .. }
        ));

        let unsupported = r#"{"protocol":2,"type":"room.close","request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","room_id":"r"}"#;
        assert!(serde_json::from_str::<SignalMessage>(unsupported)
            .unwrap()
            .validate()
            .is_err());
    }

    #[test]
    fn v1_turn_result_omits_the_optional_v2_stun_field() {
        let result = SignalMessage::TurnCredentialsResult {
            protocol: SIGNAL_PROTOCOL,
            request_id: Uuid::new_v4(),
            room_id: "room".to_owned(),
            turn_uris: vec!["turn:relay.example.test:3478?transport=udp".to_owned()],
            stun_uris: Vec::new(),
            username: "temporary".to_owned(),
            credential: "secret".to_owned(),
            expires_at: 123,
        };
        let json = serde_json::to_value(result).unwrap();
        assert!(json.get("stun_uris").is_none());
        let reparsed: SignalMessage = serde_json::from_value(json).unwrap();
        reparsed.validate().unwrap();
    }

    #[test]
    fn approved_rendezvous_examples_parse_as_v1_messages() {
        let examples = [
            include_str!("../../architecture/v1.1/examples/rendezvous/register.json"),
            include_str!("../../architecture/v1.1/examples/rendezvous/registered.json"),
            include_str!("../../architecture/v1.1/examples/rendezvous/join.json"),
            include_str!("../../architecture/v1.1/examples/rendezvous/join.accepted.json"),
            include_str!("../../architecture/v1.1/examples/rendezvous/offer.json"),
            include_str!("../../architecture/v1.1/examples/rendezvous/candidate.json"),
            include_str!("../../architecture/v1.1/examples/rendezvous/turn.credentials.result.json"),
        ];
        for example in examples {
            let message: SignalMessage = serde_json::from_str(example).unwrap();
            message.validate().unwrap();
        }
    }
}
