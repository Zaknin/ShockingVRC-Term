//! Canonical ADR-033 binding between Host identity and the negotiated DTLS peer.

use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{rand::SystemRandom, signature::{Ed25519KeyPair, KeyPair}};

    #[test]
    fn signature_is_bound_to_both_actual_dtls_fingerprints_and_room_nonce() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("keypair");
        let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("key");
        let mut fields = BindingFields::test_values();
        fields.host_id = server_id(key.public_key().as_ref());
        let signature = sign_binding(&key, &fields);
        let proof = IdentityProof::new(
            fields.clone(),
            key.public_key().as_ref().to_vec(),
            signature,
        );
        let pin = identity_fingerprint(key.public_key().as_ref());

        assert!(verify_binding(&proof, &fields, &pin).is_ok());

        let mut changed = fields.clone();
        changed.host_dtls_fingerprint[0] ^= 1;
        assert!(verify_binding(&proof, &changed, &pin).is_err());

        let mut changed = fields.clone();
        changed.room_nonce[0] ^= 1;
        assert!(verify_binding(&proof, &changed, &pin).is_err());

        let mut wrong_pin = pin.clone();
        wrong_pin.replace_range(0..2, "00");
        assert!(verify_binding(&proof, &fields, &wrong_pin).is_err());
    }

    fn sign_binding(key: &Ed25519KeyPair, fields: &BindingFields) -> Vec<u8> {
        key.sign(&binding_transcript(fields).expect("transcript")).as_ref().to_vec()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BindingFields {
    pub host_id: String,
    pub server_epoch: uuid::Uuid,
    pub registration_epoch: uuid::Uuid,
    pub request_id: uuid::Uuid,
    pub room_id: uuid::Uuid,
    pub room_nonce: [u8; 32],
    pub client_nonce: [u8; 32],
    pub server_nonce: [u8; 32],
    pub host_dtls_fingerprint: [u8; 32],
    pub client_dtls_fingerprint: [u8; 32],
    pub resume_session_id: Option<uuid::Uuid>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct IdentityProof {
    #[serde(rename = "type")]
    pub message_type: String,
    pub protocol: u8,
    #[serde(flatten)]
    pub fields: BindingFields,
    pub public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

impl IdentityProof {
    pub fn new(fields: BindingFields, public_key: Vec<u8>, signature: Vec<u8>) -> Self {
        Self {
            message_type: "rtc.identity.proof".to_owned(),
            protocol: 1,
            fields,
            public_key,
            signature,
        }
    }
}

pub fn binding_transcript(fields: &BindingFields) -> Result<Vec<u8>, BindingError> {
    let host_id = fields.host_id.as_bytes();
    let host_id_len = u32::try_from(host_id.len()).map_err(|_| BindingError::FieldTooLong)?;
    let mut transcript = Vec::with_capacity(180 + host_id.len());
    transcript.extend_from_slice(b"ShockingVRC-Term/ADR-033/rtc-binding/v1\0");
    transcript.push(1);
    transcript.extend_from_slice(&host_id_len.to_be_bytes());
    transcript.extend_from_slice(host_id);
    transcript.extend_from_slice(fields.server_epoch.as_bytes());
    transcript.extend_from_slice(fields.registration_epoch.as_bytes());
    transcript.extend_from_slice(fields.request_id.as_bytes());
    transcript.extend_from_slice(fields.room_id.as_bytes());
    transcript.extend_from_slice(&fields.room_nonce);
    transcript.extend_from_slice(&fields.client_nonce);
    transcript.extend_from_slice(&fields.server_nonce);
    transcript.extend_from_slice(b"webrtc-datachannel\0");
    transcript.extend_from_slice(&fields.host_dtls_fingerprint);
    transcript.extend_from_slice(&fields.client_dtls_fingerprint);
    match fields.resume_session_id {
        Some(session_id) => {
            transcript.push(1);
            transcript.extend_from_slice(session_id.as_bytes());
        }
        None => transcript.push(0),
    }
    Ok(transcript)
}

pub fn identity_fingerprint(public_key: &[u8]) -> String {
    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut spki = Vec::with_capacity(ED25519_SPKI_PREFIX.len() + public_key.len());
    spki.extend_from_slice(&ED25519_SPKI_PREFIX);
    spki.extend_from_slice(public_key);
    hex_lower(&sha2::Sha256::digest(spki))
}

pub fn server_id(public_key: &[u8]) -> String {
    data_encoding::BASE32_NOPAD
        .encode(&sha2::Sha256::digest(public_key))
        .to_ascii_lowercase()
}

pub fn verify_binding(
    proof: &IdentityProof,
    expected_fields: &BindingFields,
    expected_fingerprint: &str,
) -> Result<(), BindingError> {
    use ring::signature::{ED25519, UnparsedPublicKey};

    if proof.message_type != "rtc.identity.proof"
        || proof.protocol != 1
        || proof.fields != *expected_fields
        || proof.public_key.len() != 32
        || server_id(&proof.public_key) != expected_fields.host_id
    {
        return Err(BindingError::TranscriptMismatch);
    }
    if identity_fingerprint(&proof.public_key) != expected_fingerprint.to_ascii_lowercase() {
        return Err(BindingError::FingerprintMismatch);
    }
    let transcript = binding_transcript(&proof.fields)?;
    UnparsedPublicKey::new(&ED25519, &proof.public_key)
        .verify(&transcript, &proof.signature)
        .map_err(|_| BindingError::SignatureMismatch)
}

#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    #[error("Host identity fingerprint does not match the saved pin")]
    FingerprintMismatch,
    #[error("Host identity proof signature is invalid")]
    SignatureMismatch,
    #[error("Host identity proof does not match the negotiated connection")]
    TranscriptMismatch,
    #[error("Host identity public key is invalid")]
    InvalidPublicKey,
    #[error("Host identity binding field is too long")]
    FieldTooLong,
}

#[cfg(test)]
impl BindingFields {
    fn test_values() -> Self {
        Self {
            host_id: String::new(),
            server_epoch: uuid::Uuid::new_v4(),
            registration_epoch: uuid::Uuid::new_v4(),
            request_id: uuid::Uuid::new_v4(),
            room_id: uuid::Uuid::new_v4(),
            room_nonce: [1; 32],
            client_nonce: [2; 32],
            server_nonce: [3; 32],
            host_dtls_fingerprint: [4; 32],
            client_dtls_fingerprint: [5; 32],
            resume_session_id: None,
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}
