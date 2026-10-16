use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

const INVITATION_PREFIX: &str = "shockingvrc://pair/";
const DEVICE_GRANT_PREFIX: &str = "dg1.";
const MAX_INVITATION_BYTES: usize = 12 * 1024;
const MAX_DEVICE_GRANT_BYTES: usize = 4096;
const PAIRING_TTL_SECONDS: u64 = 900;
const DEVICE_ID_DOMAIN: &[u8] = b"ShockingVRC/terminal-device-id/v1\0";
const INVITATION_DOMAIN: &[u8] = b"ShockingVRC/pair-invitation/v1\0";
const DEVICE_GRANT_DOMAIN: &[u8] = b"ShockingVRC/device-grant/v1\0";
const DEVICE_AUTH_DOMAIN: &[u8] = b"ShockingVRC/device-auth/v1\0";
const ROUTING_DOMAIN: &[u8] = b"ShockingVRC/rendezvous-route/v1\0";
const ED25519_SPKI_PREFIX: &[u8] = &[0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PairingError {
    #[error("pairing invitation is malformed")]
    Malformed,
    #[error("pairing invitation has expired or has an invalid lifetime")]
    Expired,
    #[error("pairing invitation Host identity or signature is invalid")]
    HostIdentity,
    #[error("pairing invitation routing data is invalid")]
    Routing,
    #[error("Terminal device grant is invalid")]
    DeviceGrant,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingInvitationV1 {
    pub version: u8,
    pub host_id: String,
    pub host_public_key: String,
    pub host_spki_der: String,
    pub host_fingerprint: String,
    pub direct_host: Option<String>,
    pub direct_port: Option<u16>,
    pub rendezvous_url: Option<String>,
    pub ice_server_urls: Vec<String>,
    pub invitation_id: Uuid,
    pub pairing_secret: String,
    pub route_token: Option<String>,
    pub issued_at: u64,
    pub expires_at: u64,
    pub signature: String,
}

impl Drop for PairingInvitationV1 {
    fn drop(&mut self) {
        self.pairing_secret.zeroize();
        self.route_token.zeroize();
    }
}

impl std::fmt::Debug for PairingInvitationV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingInvitationV1")
            .field("version", &self.version)
            .field("host_id", &self.host_id)
            .field("invitation_id", &self.invitation_id)
            .field("expires_at", &self.expires_at)
            .field("pairing_secret", &"<redacted>")
            .field("route_token", &self.route_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl PairingInvitationV1 {
    /// Stable binary transcript; `signature` is intentionally excluded.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(512);
        bytes.extend_from_slice(INVITATION_DOMAIN);
        bytes.push(self.version);
        append_field(&mut bytes, self.host_id.as_bytes());
        append_field(&mut bytes, self.host_public_key.as_bytes());
        append_field(&mut bytes, self.host_spki_der.as_bytes());
        append_field(&mut bytes, self.host_fingerprint.as_bytes());
        append_optional_field(&mut bytes, self.direct_host.as_deref());
        bytes.extend_from_slice(&self.direct_port.unwrap_or_default().to_be_bytes());
        append_optional_field(&mut bytes, self.rendezvous_url.as_deref());
        bytes.extend_from_slice(&(self.ice_server_urls.len() as u16).to_be_bytes());
        for url in &self.ice_server_urls {
            append_field(&mut bytes, url.as_bytes());
        }
        bytes.extend_from_slice(self.invitation_id.as_bytes());
        append_field(&mut bytes, self.pairing_secret.as_bytes());
        append_optional_field(&mut bytes, self.route_token.as_deref());
        bytes.extend_from_slice(&self.issued_at.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        bytes
    }

    pub fn verify(&self, now: u64) -> Result<(), PairingError> {
        if self.version != 1 || self.host_id.is_empty() || self.host_id.len() > 256 {
            return Err(PairingError::Malformed);
        }
        if self.expires_at <= now
            || self.expires_at <= self.issued_at
            || self.expires_at - self.issued_at != PAIRING_TTL_SECONDS
        {
            return Err(PairingError::Expired);
        }
        let public_key = decode_fixed::<32>(&self.host_public_key).ok_or(PairingError::HostIdentity)?;
        if host_id_for_public_key(&public_key)? != self.host_id {
            return Err(PairingError::HostIdentity);
        }
        let spki_der = decode(&self.host_spki_der).ok_or(PairingError::HostIdentity)?;
        if spki_der.len() != ED25519_SPKI_PREFIX.len() + public_key.len()
            || !spki_der.starts_with(ED25519_SPKI_PREFIX)
            || spki_der[ED25519_SPKI_PREFIX.len()..] != public_key
            || hex_lower(&Sha256::digest(&spki_der)) != self.host_fingerprint
        {
            return Err(PairingError::HostIdentity);
        }
        let pairing_secret = decode_fixed::<32>(&self.pairing_secret).ok_or(PairingError::Malformed)?;
        if pairing_secret == [0; 32] {
            return Err(PairingError::Malformed);
        }
        match (&self.direct_host, self.direct_port) {
            (Some(host), Some(port)) if port > 0 => validate_host(host)?,
            (None, None) => {}
            _ => return Err(PairingError::Routing),
        }
        if let Some(url) = &self.rendezvous_url {
            validate_rendezvous_url(url)?;
        }
        if self.route_token.is_some() != self.rendezvous_url.is_some() {
            return Err(PairingError::Routing);
        }
        if self.direct_host.is_none() && self.rendezvous_url.is_none() {
            return Err(PairingError::Routing);
        }
        if self.ice_server_urls.len() > 16 || self.ice_server_urls.iter().any(|url| !validate_ice_uri(url)) {
            return Err(PairingError::Routing);
        }
        if let Some(token) = &self.route_token {
            validate_route_claims(token, self.invitation_id, self.expires_at)?;
        }
        let signature = decode_fixed::<64>(&self.signature).ok_or(PairingError::HostIdentity)?;
        UnparsedPublicKey::new(&ED25519, &public_key)
            .verify(&self.signing_bytes(), &signature)
            .map_err(|_| PairingError::HostIdentity)
    }

    pub fn to_uri(&self, now: u64) -> Result<String, PairingError> {
        self.verify(now)?;
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(self).map_err(|_| PairingError::Malformed)?);
        if bytes.len() > MAX_INVITATION_BYTES {
            return Err(PairingError::Malformed);
        }
        Ok(format!("{INVITATION_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes.as_slice())))
    }

    pub fn parse_uri(uri: &str, now: u64) -> Result<Self, PairingError> {
        let payload = uri.strip_prefix(INVITATION_PREFIX).ok_or(PairingError::Malformed)?;
        if payload.is_empty() || payload.len() > MAX_INVITATION_BYTES * 2 || payload.contains(['/', '?', '#', '%']) {
            return Err(PairingError::Malformed);
        }
        let bytes = zeroize::Zeroizing::new(URL_SAFE_NO_PAD.decode(payload).map_err(|_| PairingError::Malformed)?);
        if bytes.len() > MAX_INVITATION_BYTES {
            return Err(PairingError::Malformed);
        }
        let invitation: Self = serde_json::from_slice(bytes.as_slice()).map_err(|_| PairingError::Malformed)?;
        invitation.verify(now)?;
        Ok(invitation)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceGrantV1 {
    pub version: u8,
    pub host_id: String,
    pub device_id: String,
    pub terminal_public_key: String,
    pub signature: String,
}

impl DeviceGrantV1 {
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(DEVICE_GRANT_DOMAIN);
        bytes.push(self.version);
        append_field(&mut bytes, self.host_id.as_bytes());
        append_field(&mut bytes, self.device_id.as_bytes());
        append_field(&mut bytes, self.terminal_public_key.as_bytes());
        bytes
    }

    pub fn to_authorization(&self) -> Result<String, PairingError> {
        if self.version != 1 || decode_fixed::<64>(&self.signature).is_none() {
            return Err(PairingError::DeviceGrant);
        }
        let payload = serde_json::to_vec(self).map_err(|_| PairingError::DeviceGrant)?;
        let value = format!("{DEVICE_GRANT_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload));
        if value.len() > MAX_DEVICE_GRANT_BYTES {
            return Err(PairingError::DeviceGrant);
        }
        Ok(value)
    }
}

pub fn verify_device_grant(
    authorization: &str,
    expected_host_id: &str,
    registered_host_public_key: &[u8],
) -> Result<DeviceGrantV1, PairingError> {
    let encoded = authorization
        .strip_prefix(DEVICE_GRANT_PREFIX)
        .ok_or(PairingError::DeviceGrant)?;
    if encoded.len() > MAX_DEVICE_GRANT_BYTES {
        return Err(PairingError::DeviceGrant);
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| PairingError::DeviceGrant)?;
    let grant: DeviceGrantV1 = serde_json::from_slice(&bytes).map_err(|_| PairingError::DeviceGrant)?;
    let terminal_public_key = decode_fixed::<32>(&grant.terminal_public_key).ok_or(PairingError::DeviceGrant)?;
    let signature = decode_fixed::<64>(&grant.signature).ok_or(PairingError::DeviceGrant)?;
    if grant.version != 1
        || grant.host_id != expected_host_id
        || host_id_for_public_key(registered_host_public_key)? != expected_host_id
        || grant.device_id != device_id_for_public_key(&terminal_public_key)?
    {
        return Err(PairingError::DeviceGrant);
    }
    UnparsedPublicKey::new(&ED25519, registered_host_public_key)
        .verify(&grant.signing_bytes(), &signature)
        .map_err(|_| PairingError::DeviceGrant)?;
    Ok(grant)
}

pub fn device_auth_transcript(
    host_id: &str,
    server_epoch: Uuid,
    request_id: Uuid,
    challenge_id: Uuid,
    server_nonce: [u8; 32],
    device_id: &str,
    public_key: [u8; 32],
    invitation_id: Option<Uuid>,
    pairing_secret_hash: Option<[u8; 32]>,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    bytes.extend_from_slice(DEVICE_AUTH_DOMAIN);
    append_field(&mut bytes, host_id.as_bytes());
    bytes.extend_from_slice(server_epoch.as_bytes());
    bytes.extend_from_slice(request_id.as_bytes());
    bytes.extend_from_slice(challenge_id.as_bytes());
    bytes.extend_from_slice(&server_nonce);
    append_field(&mut bytes, device_id.as_bytes());
    bytes.extend_from_slice(&public_key);
    if let Some(invitation_id) = invitation_id {
        bytes.push(1);
        bytes.extend_from_slice(invitation_id.as_bytes());
    } else {
        bytes.push(0);
    }
    if let Some(secret_hash) = pairing_secret_hash {
        bytes.push(1);
        bytes.extend_from_slice(&secret_hash);
    } else {
        bytes.push(0);
    }
    bytes
}

pub fn host_id_for_public_key(public_key: &[u8]) -> Result<String, PairingError> {
    if public_key.len() != 32 {
        return Err(PairingError::HostIdentity);
    }
    Ok(BASE32_NOPAD.encode(&Sha256::digest(public_key)).to_ascii_lowercase())
}

pub fn verify_host_identity_key(host_id: &str, encoded_public_key: &str) -> Result<[u8; 32], PairingError> {
    let public_key = decode_fixed::<32>(encoded_public_key).ok_or(PairingError::HostIdentity)?;
    if host_id_for_public_key(&public_key)? != host_id {
        return Err(PairingError::HostIdentity);
    }
    Ok(public_key)
}

pub fn device_id_for_public_key(public_key: &[u8]) -> Result<String, PairingError> {
    if public_key.len() != 32 {
        return Err(PairingError::DeviceGrant);
    }
    Ok(format!(
        "d1_{}",
        BASE32_NOPAD
            .encode(&Sha256::digest([DEVICE_ID_DOMAIN, public_key].concat()))
            .to_ascii_lowercase()
    ))
}

pub fn create_route_token(registration_key: &[u8], host_id: &str, invitation_id: Uuid, expires_at: u64) -> String {
    let mut mac = HmacSha256::new_from_slice(registration_key).expect("HMAC accepts any key size");
    mac.update(ROUTING_DOMAIN);
    append_field_hmac(&mut mac, host_id.as_bytes());
    mac.update(invitation_id.as_bytes());
    mac.update(&expires_at.to_be_bytes());
    format!(
        "r1.{invitation_id}.{expires_at}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

pub fn verify_route_token(
    registration_key: &[u8],
    host_id: &str,
    token: &str,
    now: u64,
) -> Result<(Uuid, u64), PairingError> {
    let mut parts = token.split('.');
    if parts.next() != Some("r1") {
        return Err(PairingError::Routing);
    }
    let invitation_id =
        Uuid::parse_str(parts.next().ok_or(PairingError::Routing)?).map_err(|_| PairingError::Routing)?;
    let expires_at = parts
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(PairingError::Routing)?;
    let signature = parts.next().ok_or(PairingError::Routing)?;
    if parts.next().is_some() || expires_at <= now {
        return Err(PairingError::Expired);
    }
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| PairingError::Routing)?;
    let mut mac = HmacSha256::new_from_slice(registration_key).expect("HMAC accepts any key size");
    mac.update(ROUTING_DOMAIN);
    append_field_hmac(&mut mac, host_id.as_bytes());
    mac.update(invitation_id.as_bytes());
    mac.update(&expires_at.to_be_bytes());
    mac.verify_slice(&signature).map_err(|_| PairingError::Routing)?;
    Ok((invitation_id, expires_at))
}

pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode(value: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value).ok()
}

fn decode_fixed<const N: usize>(value: &str) -> Option<[u8; N]> {
    decode(value)?.try_into().ok()
}

fn append_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(u32::try_from(value.len()).unwrap_or(u32::MAX)).to_be_bytes());
    output.extend_from_slice(value);
}

fn append_optional_field(output: &mut Vec<u8>, value: Option<&str>) {
    if let Some(value) = value {
        output.push(1);
        append_field(output, value.as_bytes());
    } else {
        output.push(0);
    }
}

fn append_field_hmac(mac: &mut HmacSha256, value: &[u8]) {
    mac.update(&(u32::try_from(value.len()).unwrap_or(u32::MAX)).to_be_bytes());
    mac.update(value);
}

fn validate_host(host: &str) -> Result<(), PairingError> {
    if host.is_empty()
        || host.len() > 255
        || host.chars().any(char::is_whitespace)
        || host.contains(['/', '@', '#', '?', '[', ']'])
    {
        return Err(PairingError::Routing);
    }
    Ok(())
}

fn validate_rendezvous_url(value: &str) -> Result<(), PairingError> {
    let url = url::Url::parse(value).map_err(|_| PairingError::Routing)?;
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
    });
    if !(url.scheme() == "wss" || (url.scheme() == "ws" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PairingError::Routing);
    }
    Ok(())
}

fn validate_ice_uri(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.chars().any(char::is_whitespace)
        && !value.contains('@')
        && (value.starts_with("stun:")
            || value.starts_with("stuns:")
            || value.starts_with("turn:")
            || value.starts_with("turns:"))
}

fn validate_route_claims(token: &str, invitation_id: Uuid, expires_at: u64) -> Result<(), PairingError> {
    let mut parts = token.split('.');
    if parts.next() != Some("r1")
        || parts.next().and_then(|value| Uuid::parse_str(value).ok()) != Some(invitation_id)
        || parts.next().and_then(|value| value.parse::<u64>().ok()) != Some(expires_at)
        || parts
            .next()
            .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
            .is_none_or(|signature| signature.len() != 32)
        || parts.next().is_some()
    {
        return Err(PairingError::Routing);
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::{
        DeviceGrantV1, PairingInvitationV1, create_route_token, device_auth_transcript, device_id_for_public_key,
        encode, host_id_for_public_key, verify_device_grant, verify_route_token,
    };

    fn key_pair() -> Ed25519KeyPair {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("test Ed25519 key");
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("parse test Ed25519 key")
    }

    fn signed_invitation(now: u64) -> PairingInvitationV1 {
        let host = key_pair();
        let host_key = host.public_key().as_ref();
        let mut spki = vec![0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        spki.extend_from_slice(host_key);
        let fingerprint = Sha256::digest(&spki)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let invitation_id = Uuid::new_v4();
        let host_id = host_id_for_public_key(host_key).expect("Host id");
        let expires_at = now + 900;
        let mut invitation = PairingInvitationV1 {
            version: 1,
            host_id: host_id.clone(),
            host_public_key: encode(host_key),
            host_spki_der: encode(&spki),
            host_fingerprint: fingerprint,
            direct_host: Some("127.0.0.1".to_owned()),
            direct_port: Some(4318),
            rendezvous_url: Some("wss://signal.example.invalid/api/signal/v1".to_owned()),
            ice_server_urls: vec!["stun:stun.example.invalid:3478".to_owned()],
            invitation_id,
            pairing_secret: encode(&[7_u8; 32]),
            route_token: Some(create_route_token(
                b"local-test-registration-key",
                &host_id,
                invitation_id,
                expires_at,
            )),
            issued_at: now,
            expires_at,
            signature: String::new(),
        };
        invitation.signature = encode(host.sign(&invitation.signing_bytes()).as_ref());
        invitation
    }

    #[test]
    fn pairing_invitation_rejects_expired_and_wrong_signature() {
        let now = 1_800_000_000;
        let invite = signed_invitation(now);
        assert!(invite.verify(now).is_ok());
        assert!(invite.verify(now + 901).is_err());
        let mut tampered = invite;
        tampered.direct_port = Some(4319);
        assert!(tampered.verify(now).is_err());
    }

    #[test]
    fn pairing_invitation_binds_host_identity_and_routing_fields() {
        let now = 1_800_000_000;
        let invite = signed_invitation(now);
        let uri = invite.to_uri(now).expect("signed invite URI");
        let parsed = PairingInvitationV1::parse_uri(&uri, now).expect("validated invitation");
        assert_eq!(parsed.invitation_id, invite.invitation_id);
        assert_eq!(parsed.host_id, invite.host_id);
        assert_eq!(parsed.route_token, invite.route_token);
        assert_eq!(parsed.pairing_secret, invite.pairing_secret);
        assert!(PairingInvitationV1::parse_uri("https://example.invalid/pair", now).is_err());
        assert!(PairingInvitationV1::parse_uri(&format!("{uri}x"), now).is_err());
    }

    #[test]
    fn routing_token_is_per_invitation_and_expires() {
        let key = b"local-test-registration-key";
        let host_id = "host-id";
        let id = Uuid::new_v4();
        let expires_at = 1_800_000_900;
        let token = create_route_token(key, host_id, id, expires_at);
        assert_eq!(verify_route_token(key, host_id, &token, 1_800_000_000), Ok((id, expires_at)));
        assert!(verify_route_token(key, "another-host", &token, 1_800_000_000).is_err());
        assert!(verify_route_token(key, host_id, &token, expires_at).is_err());
        assert_ne!(token, create_route_token(key, host_id, Uuid::new_v4(), expires_at));
    }

    #[test]
    fn device_grant_binds_host_and_terminal_key() {
        let host = key_pair();
        let terminal = key_pair();
        let host_id = host_id_for_public_key(host.public_key().as_ref()).expect("Host id");
        let terminal_public_key = terminal.public_key().as_ref();
        let mut grant = DeviceGrantV1 {
            version: 1,
            host_id: host_id.clone(),
            device_id: device_id_for_public_key(terminal_public_key).expect("device id"),
            terminal_public_key: encode(terminal_public_key),
            signature: String::new(),
        };
        grant.signature = encode(host.sign(&grant.signing_bytes()).as_ref());
        let authorization = grant.to_authorization().expect("grant encoding");
        assert!(verify_device_grant(&authorization, &host_id, host.public_key().as_ref()).is_ok());
        assert!(verify_device_grant(&authorization, "other-host", host.public_key().as_ref()).is_err());
        assert!(verify_device_grant(&authorization, &host_id, key_pair().public_key().as_ref()).is_err());
    }

    #[test]
    fn device_auth_transcript_binds_challenge_and_pair_secret() {
        let host_id = "host-id";
        let epoch = Uuid::new_v4();
        let request = Uuid::new_v4();
        let challenge = Uuid::new_v4();
        let device_id = "device-id";
        let public_key = [9_u8; 32];
        let nonce = [8_u8; 32];
        let invitation_id = Some(Uuid::new_v4());
        let secret_hash = Some([7_u8; 32]);
        let transcript = device_auth_transcript(
            host_id,
            epoch,
            request,
            challenge,
            nonce,
            device_id,
            public_key,
            invitation_id,
            secret_hash,
        );
        assert_ne!(
            transcript,
            device_auth_transcript(
                host_id,
                epoch,
                request,
                Uuid::new_v4(),
                nonce,
                device_id,
                public_key,
                invitation_id,
                secret_hash
            )
        );
        assert_ne!(
            transcript,
            device_auth_transcript(
                host_id,
                epoch,
                request,
                challenge,
                nonce,
                device_id,
                public_key,
                invitation_id,
                Some([6_u8; 32])
            )
        );
    }
}
