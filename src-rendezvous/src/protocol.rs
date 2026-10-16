use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const SIGNAL_PROTOCOL: u8 = 1;

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
    },
    #[serde(rename = "join.accepted")]
    JoinAccepted {
        protocol: u8,
        request_id: Uuid,
        host_id: String,
        room_id: String,
        nonce: String,
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
        if self.protocol() != SIGNAL_PROTOCOL {
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
