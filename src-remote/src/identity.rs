use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use data_encoding::BASE32_NOPAD;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519, SigningKey};
use rustls::pki_types::PrivatePkcs8KeyDer;
use sha2::{Digest, Sha256};
use thiserror::Error;
use x509_parser::{certificate::X509Certificate, prelude::FromDer};
use zeroize::Zeroizing;

const IDENTITY_MAGIC: &[u8] = b"SVRC-HOST-IDENTITY\x01";
const REGISTRATION_KEY_MAGIC: &[u8] = b"SVRC-RENDEZVOUS-KEY\x01";

#[derive(Error, Debug)]
pub enum IdentityError {
    #[error("Host identity storage is unavailable: {0}")]
    Storage(std::io::Error),
    #[error("Windows DPAPI failed: {0}")]
    Dpapi(std::io::Error),
    #[error("stored Host identity is corrupt or incompatible")]
    Corrupt,
    #[error("Host identity operation is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("Host identity certificate generation failed: {0}")]
    Certificate(String),
}

/// Stable Ed25519 Host identity. Private key bytes remain in user-protected memory
/// and are only persisted as a Windows DPAPI blob.
pub struct HostIdentity {
    private_key_der: Zeroizing<Vec<u8>>,
    certificate_der: Vec<u8>,
    spki_der: Vec<u8>,
    public_key: Vec<u8>,
    fingerprint: String,
    server_id: String,
}

impl HostIdentity {
    #[cfg(test)]
    pub(crate) fn ephemeral_for_test() -> Self {
        let key_pair = KeyPair::generate_for(&PKCS_ED25519).expect("ephemeral test Host key");
        let private_key_der = Zeroizing::new(key_pair.serialize_der());
        Self::from_parts(private_key_der, key_pair).expect("ephemeral test Host identity")
    }

    pub fn load_or_create() -> Result<Self, IdentityError> {
        let path = identity_path()?;
        match fs::read(&path) {
            Ok(protected) => {
                let mut plaintext = Zeroizing::new(dpapi_unprotect(&protected)?);
                if !plaintext.starts_with(IDENTITY_MAGIC) {
                    return Err(IdentityError::Corrupt);
                }
                let key_der = Zeroizing::new(plaintext.split_off(IDENTITY_MAGIC.len()));
                Self::from_private_key(key_der)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key_pair = KeyPair::generate_for(&PKCS_ED25519)
                    .map_err(|error| IdentityError::Certificate(error.to_string()))?;
                let private_key_der = Zeroizing::new(key_pair.serialize_der());
                let identity = Self::from_parts(private_key_der, key_pair)?;
                let mut plaintext =
                    Zeroizing::new(Vec::with_capacity(IDENTITY_MAGIC.len() + identity.private_key_der.len()));
                plaintext.extend_from_slice(IDENTITY_MAGIC);
                plaintext.extend_from_slice(&identity.private_key_der);
                let protected = dpapi_protect(&plaintext)?;
                write_new_identity(&path, &protected)?;
                Ok(identity)
            }
            Err(error) => Err(IdentityError::Storage(error)),
        }
    }

    fn from_private_key(private_key_der: Zeroizing<Vec<u8>>) -> Result<Self, IdentityError> {
        let key_der = PrivatePkcs8KeyDer::from(private_key_der.as_slice());
        let key_pair =
            KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &PKCS_ED25519).map_err(|_| IdentityError::Corrupt)?;
        Self::from_parts(private_key_der, key_pair)
    }

    fn from_parts(private_key_der: Zeroizing<Vec<u8>>, key_pair: KeyPair) -> Result<Self, IdentityError> {
        let params = CertificateParams::new(vec!["localhost".to_owned()])
            .map_err(|error| IdentityError::Certificate(error.to_string()))?;
        let certificate_der = params
            .self_signed(&key_pair)
            .map_err(|error| IdentityError::Certificate(error.to_string()))?
            .der()
            .clone();

        let (_, certificate) =
            X509Certificate::from_der(certificate_der.as_ref()).map_err(|_| IdentityError::Corrupt)?;
        if certificate.subject() != certificate.issuer() || certificate.verify_signature(None).is_err() {
            return Err(IdentityError::Corrupt);
        }

        let spki_der = certificate.tbs_certificate.subject_pki.raw.to_vec();
        let public_key = certificate.tbs_certificate.subject_pki.subject_public_key.data.to_vec();
        let fingerprint = hex_lower(&Sha256::digest(&spki_der));
        let server_id = BASE32_NOPAD.encode(&Sha256::digest(&public_key)).to_ascii_lowercase();

        Ok(Self {
            private_key_der,
            certificate_der: certificate_der.as_ref().to_vec(),
            spki_der,
            public_key,
            fingerprint,
            server_id,
        })
    }

    pub fn certificate_der(&self) -> Vec<Vec<u8>> {
        vec![self.certificate_der.clone()]
    }

    pub fn private_key_der(&self) -> PrivatePkcs8KeyDer<'_> {
        PrivatePkcs8KeyDer::from(self.private_key_der.as_slice())
    }

    pub fn spki_der(&self) -> &[u8] {
        &self.spki_der
    }

    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        let key_der = PrivatePkcs8KeyDer::from(self.private_key_der.as_slice());
        let key_pair =
            KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &PKCS_ED25519).map_err(|_| IdentityError::Corrupt)?;
        key_pair
            .sign(message)
            .map_err(|error| IdentityError::Certificate(error.to_string()))
    }
}

impl shocking_vrc_rtc::HostIdentitySigner for HostIdentity {
    fn public_key(&self) -> &[u8] { self.public_key() }
    fn server_id(&self) -> &str { self.server_id() }
    fn fingerprint(&self) -> &str { self.fingerprint() }
    fn sign(&self, transcript: &[u8]) -> Result<Vec<u8>, String> {
        HostIdentity::sign(self, transcript).map_err(|error| error.to_string())
    }
}

/// Stores the rendezvous registration key as a current-Windows-user DPAPI blob.
/// An empty value clears the separately protected key file.
pub fn save_rendezvous_registration_key(value: &str) -> Result<(), IdentityError> {
    let path = registration_key_path()?;
    if value.is_empty() {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(IdentityError::Storage(error)),
        }
    }
    let mut plaintext = Zeroizing::new(Vec::with_capacity(REGISTRATION_KEY_MAGIC.len() + value.len()));
    plaintext.extend_from_slice(REGISTRATION_KEY_MAGIC);
    plaintext.extend_from_slice(value.as_bytes());
    let protected = dpapi_protect(&plaintext)?;
    let parent = path.parent().ok_or(IdentityError::UnsupportedPlatform)?;
    fs::create_dir_all(parent).map_err(IdentityError::Storage)?;
    fs::write(path, protected).map_err(IdentityError::Storage)
}

/// Loads the rendezvous registration key from its DPAPI-protected file.
pub fn load_rendezvous_registration_key() -> Result<Option<Zeroizing<String>>, IdentityError> {
    let path = registration_key_path()?;
    let protected = match fs::read(path) {
        Ok(protected) => protected,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(IdentityError::Storage(error)),
    };
    let plaintext = Zeroizing::new(dpapi_unprotect(&protected)?);
    let Some(value) = plaintext.strip_prefix(REGISTRATION_KEY_MAGIC) else { return Err(IdentityError::Corrupt); };
    let value = String::from_utf8(value.to_vec()).map_err(|_| IdentityError::Corrupt)?;
    if value.is_empty() { return Err(IdentityError::Corrupt); }
    Ok(Some(Zeroizing::new(value)))
}

fn registration_key_path() -> Result<PathBuf, IdentityError> {
    #[cfg(windows)]
    {
        let app_data = std::env::var_os("APPDATA").ok_or(IdentityError::UnsupportedPlatform)?;
        Ok(PathBuf::from(app_data).join("ShockingVRC").join("rendezvous_registration.dpapi"))
    }
    #[cfg(not(windows))]
    {
        Err(IdentityError::UnsupportedPlatform)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn identity_path() -> Result<PathBuf, IdentityError> {
    #[cfg(windows)]
    {
        let app_data = std::env::var_os("APPDATA").ok_or(IdentityError::UnsupportedPlatform)?;
        Ok(PathBuf::from(app_data).join("ShockingVRC").join("host_identity.dpapi"))
    }
    #[cfg(not(windows))]
    {
        Err(IdentityError::UnsupportedPlatform)
    }
}

fn write_new_identity(path: &PathBuf, protected: &[u8]) -> Result<(), IdentityError> {
    let directory = path.parent().ok_or(IdentityError::UnsupportedPlatform)?;
    fs::create_dir_all(directory).map_err(IdentityError::Storage)?;
    let temporary = path.with_extension(format!("dpapi.{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(IdentityError::Storage)?;
    file.write_all(protected).map_err(IdentityError::Storage)?;
    file.sync_all().map_err(IdentityError::Storage)?;
    fs::rename(&temporary, path).map_err(IdentityError::Storage)
}

#[cfg(windows)]
fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>, IdentityError> {
    use std::{ptr, slice};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData},
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext.len() as u32,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // No LOCAL_MACHINE flag: Windows protects this to the current user profile.
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(IdentityError::Dpapi(std::io::Error::last_os_error()));
    }
    let protected = unsafe { slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData as _) };
    Ok(protected)
}

#[cfg(windows)]
fn dpapi_unprotect(protected: &[u8]) -> Result<Vec<u8>, IdentityError> {
    use std::{ptr, slice};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData},
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: protected.len() as u32,
        pbData: protected.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(IdentityError::Dpapi(std::io::Error::last_os_error()));
    }
    let plaintext = unsafe { slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData as _) };
    Ok(plaintext)
}

#[cfg(not(windows))]
fn dpapi_protect(_: &[u8]) -> Result<Vec<u8>, IdentityError> {
    Err(IdentityError::UnsupportedPlatform)
}

#[cfg(not(windows))]
fn dpapi_unprotect(_: &[u8]) -> Result<Vec<u8>, IdentityError> {
    Err(IdentityError::UnsupportedPlatform)
}
