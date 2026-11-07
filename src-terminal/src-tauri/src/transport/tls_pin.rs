use std::sync::{Arc, Mutex};

use rustls::{
    ClientConfig, DigitallySignedStruct, Error, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use x509_parser::{certificate::X509Certificate, prelude::FromDer};

#[derive(Debug)]
struct SpkiPinVerifier {
    pinned_hash: [u8; 32],
    provider: Arc<CryptoProvider>,
    observed_public_key: Arc<Mutex<Option<[u8; 32]>>>,
}

impl SpkiPinVerifier {
    fn new(fingerprint: &str, provider: Arc<CryptoProvider>) -> Result<Self, String> {
        let bytes = decode_fingerprint(fingerprint)?;
        let pinned_hash: [u8; 32] = bytes.try_into().map_err(|_| "Fingerprint must be SHA-256")?;
        Ok(Self {
            pinned_hash,
            provider,
            observed_public_key: Arc::new(Mutex::new(None)),
        })
    }
}

impl ServerCertVerifier for SpkiPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let (_, certificate) = X509Certificate::from_der(end_entity.as_ref())
            .map_err(|_| Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        if certificate.subject() != certificate.issuer() || certificate.verify_signature(None).is_err() {
            return Err(Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        let observed = Sha256::digest(certificate.tbs_certificate.subject_pki.raw);
        if observed.as_slice().ct_eq(&self.pinned_hash).unwrap_u8() != 1 {
            return Err(Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        if certificate
            .tbs_certificate
            .subject_pki
            .algorithm
            .algorithm
            .to_id_string()
            != "1.3.101.112"
        {
            return Err(Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        let public_key: [u8; 32] = certificate
            .tbs_certificate
            .subject_pki
            .subject_public_key
            .data
            .as_ref()
            .try_into()
            .map_err(|_| Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        if let Ok(mut observed) = self.observed_public_key.lock() {
            *observed = Some(public_key);
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub struct PinnedTls {
    pub config: Arc<ClientConfig>,
    verifier: Arc<SpkiPinVerifier>,
}

impl PinnedTls {
    pub fn observed_public_key(&self) -> Result<[u8; 32], String> {
        self.verifier
            .observed_public_key
            .lock()
            .map_err(|_| "Host identity verifier is unavailable".to_owned())?
            .ok_or_else(|| "Host certificate did not expose its pinned Ed25519 key".to_owned())
    }
}

pub fn pinned_tls_config(fingerprint: &str) -> Result<PinnedTls, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(SpkiPinVerifier::new(fingerprint, Arc::clone(&provider))?);
    let verifier_for_rustls: Arc<dyn ServerCertVerifier> = verifier.clone();
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier_for_rustls)
        .with_no_client_auth();
    Ok(PinnedTls {
        config: Arc::new(config),
        verifier,
    })
}

pub fn decode_fingerprint(fingerprint: &str) -> Result<Vec<u8>, String> {
    let normalized = fingerprint.replace(':', "");
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Host fingerprint must be a 64-character SHA-256 SPKI hex value".to_owned());
    }
    (0..normalized.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&normalized[index..index + 2], 16).map_err(|_| "Invalid fingerprint".to_owned())
        })
        .collect()
}

pub fn canonical_fingerprint(fingerprint: &str) -> Result<String, String> {
    let bytes = decode_fingerprint(fingerprint)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
