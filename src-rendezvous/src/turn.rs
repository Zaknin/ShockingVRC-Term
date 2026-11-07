use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::{SIGNAL_PROTOCOL, SignalMessage};

type HmacSha1 = Hmac<Sha1>;

pub fn issue_credentials(
    secret: &[u8],
    ttl_seconds: u64,
    turn_uris: &[String],
    room_id: &str,
    request_id: Uuid,
) -> Result<SignalMessage, TurnError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TurnError::Clock)?;
    let expires_at = now
        .as_secs()
        .checked_add(ttl_seconds)
        .ok_or(TurnError::ExpiryOverflow)?;
    let username = format!("{expires_at}:{}", Uuid::new_v4());
    let mut mac = HmacSha1::new_from_slice(secret).map_err(|_| TurnError::InvalidSecret)?;
    mac.update(username.as_bytes());
    let credential = STANDARD.encode(mac.finalize().into_bytes());
    Ok(SignalMessage::TurnCredentialsResult {
        protocol: SIGNAL_PROTOCOL,
        request_id,
        room_id: room_id.to_owned(),
        turn_uris: turn_uris.to_vec(),
        username,
        credential,
        expires_at,
    })
}

#[derive(Debug, Error)]
pub enum TurnError {
    #[error("system clock is before the Unix epoch")]
    Clock,
    #[error("TURN credential expiry timestamp overflow")]
    ExpiryOverflow,
    #[error("TURN HMAC secret is invalid")]
    InvalidSecret,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_follow_coturn_rest_username_and_password_format() {
        let message = issue_credentials(
            &[7; 32],
            600,
            &["turn:localhost:3478?transport=udp".into()],
            "room",
            Uuid::new_v4(),
        )
        .unwrap();
        let SignalMessage::TurnCredentialsResult {
            username,
            credential,
            expires_at,
            ..
        } = message
        else {
            panic!("wrong response type")
        };
        let (timestamp, member_id) = username.split_once(':').unwrap();
        assert_eq!(timestamp.parse::<u64>().unwrap(), expires_at);
        assert!(Uuid::parse_str(member_id).is_ok());
        assert_eq!(STANDARD.decode(credential).unwrap().len(), 20);
    }

    #[test]
    fn hmac_sha1_matches_known_turn_rest_primitive_vector() {
        let mut mac = HmacSha1::new_from_slice(b"key").unwrap();
        mac.update(b"The quick brown fox jumps over the lazy dog");
        let actual = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(actual, "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9");
    }
}
