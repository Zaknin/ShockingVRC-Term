use std::sync::Arc;

use axum_server::tls_rustls::RustlsConfig;
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    server::ServerConfig,
};

use crate::{HostIdentity, IdentityError};

pub fn server_config(identity: &HostIdentity) -> Result<RustlsConfig, TlsConfigError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let private_key = PrivateKeyDer::Pkcs8(identity.private_key_der().clone_key());
    let certificates: Vec<CertificateDer<'static>> = identity
        .certificate_der()
        .into_iter()
        .map(CertificateDer::from)
        .collect();
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| TlsConfigError::Rustls(error.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| TlsConfigError::Rustls(error.to_string()))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(RustlsConfig::from_config(Arc::new(config)))
}

#[derive(Debug, thiserror::Error)]
pub enum TlsConfigError {
    #[error("Host identity is not available: {0}")]
    Identity(#[from] IdentityError),
    #[error("rustls could not configure the Host certificate: {0}")]
    Rustls(String),
}
