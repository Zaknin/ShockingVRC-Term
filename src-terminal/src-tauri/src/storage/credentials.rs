use uuid::Uuid;
use zeroize::Zeroizing;
use ring::{rand::SystemRandom, signature::{Ed25519KeyPair, KeyPair}};
use shocking_vrc_rendezvous::pairing::device_id_for_public_key;

const MAX_CREDENTIAL_BYTES: usize = 5_120;
const DEVICE_KEY_TARGET: &str = "ShockingVRC.Terminal/device-key/v1";
const GRANT_PREFIX: &str = "ShockingVRC.Terminal/device-grant/";

pub struct TerminalDeviceIdentity {
    private_pkcs8: Zeroizing<Vec<u8>>,
    pub public_key: [u8; 32],
    pub device_id: String,
}

impl TerminalDeviceIdentity {
    pub fn load_or_create() -> Result<Self, String> {
        let private_pkcs8 = match read_windows(DEVICE_KEY_TARGET)? {
            Some((_, value)) => value,
            None => {
                let generated = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                    .map_err(|_| "Could not generate the Terminal device key".to_owned())?;
                let value = Zeroizing::new(generated.as_ref().to_vec());
                write_windows(DEVICE_KEY_TARGET, "device-key", &value)?;
                value
            }
        };
        let key = Ed25519KeyPair::from_pkcs8(private_pkcs8.as_ref())
            .map_err(|_| "The saved Terminal device key is invalid".to_owned())?;
        let public_key: [u8; 32] = key.public_key().as_ref().try_into()
            .map_err(|_| "The saved Terminal device key has an invalid public key".to_owned())?;
        let device_id = device_id_for_public_key(&public_key).map_err(|error| error.to_string())?;
        Ok(Self { private_pkcs8, public_key, device_id })
    }

    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        let key = Ed25519KeyPair::from_pkcs8(self.private_pkcs8.as_ref())
            .map_err(|_| "The saved Terminal device key is invalid".to_owned())?;
        Ok(key.sign(message).as_ref().to_vec())
    }
}

pub fn save_device_grant(profile_id: &str, fingerprint: &str, grant: &str) -> Result<(), String> {
    if fingerprint.len() != 64 || grant.is_empty() || grant.len() > 4096 {
        return Err("The Host device grant is invalid".to_owned());
    }
    let mut blob = Zeroizing::new(Vec::with_capacity(65 + grant.len()));
    blob.extend_from_slice(fingerprint.as_bytes());
    blob.push(0);
    blob.extend_from_slice(grant.as_bytes());
    write_windows(&grant_target_name(profile_id)?, "device-grant", &blob)
}

pub fn read_device_grant(profile_id: &str, fingerprint: &str) -> Result<Option<Zeroizing<String>>, String> {
    let Some((_, blob)) = read_windows(&grant_target_name(profile_id)?)? else { return Ok(None); };
    if fingerprint.len() != 64 || blob.len() < 66 || blob.get(64) != Some(&0) || &blob[..64] != fingerprint.as_bytes() {
        return Ok(None);
    }
    let grant = std::str::from_utf8(&blob[65..]).map_err(|_| "Saved Terminal device grant is invalid".to_owned())?;
    if !grant.starts_with("dg1.") { return Ok(None); }
    Ok(Some(Zeroizing::new(grant.to_owned())))
}

fn grant_target_name(profile_id: &str) -> Result<String, String> {
    let id = Uuid::parse_str(profile_id).map_err(|_| "Host profile ID is invalid".to_owned())?;
    Ok(format!("{GRANT_PREFIX}{id}"))
}

pub fn delete(profile_id: &str) -> Result<(), String> {
    delete_windows(&grant_target_name(profile_id)?)
}

#[cfg(windows)]
fn write_windows(target: &str, username: &str, secret: &[u8]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
    };

    let mut target_wide: Vec<u16> = std::ffi::OsStr::new(target).encode_wide().collect();
    target_wide.push(0);
    let mut username_wide: Vec<u16> = std::ffi::OsStr::new(username).encode_wide().collect();
    username_wide.push(0);
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target_wide.as_mut_ptr(),
        CredentialBlobSize: u32::try_from(secret.len()).map_err(|_| "Saved Terminal device credential is too large".to_owned())?,
        CredentialBlob: secret.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username_wide.as_mut_ptr(),
        ..Default::default()
    };
    // SAFETY: all pointers reference NUL-terminated buffers or a byte slice that remain alive for this call.
    let written = unsafe { CredWriteW(&credential, 0) };
    if written == 0 {
        Err(format!("Windows Credential Manager could not save the Terminal device credential: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn write_windows(_target: &str, _username: &str, _secret: &[u8]) -> Result<(), String> {
    Err("Terminal device credentials are available only on Windows".to_owned())
}

#[cfg(windows)]
fn read_windows(target: &str) -> Result<Option<(String, Zeroizing<Vec<u8>>)>, String> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW},
    };

    let mut target_wide: Vec<u16> = std::ffi::OsStr::new(target).encode_wide().collect();
    target_wide.push(0);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: target_wide is NUL-terminated and credential is a valid output pointer.
    let read_ok = unsafe { CredReadW(target_wide.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if read_ok == 0 {
        let error = unsafe { GetLastError() };
        if error == 1168 {
            return Ok(None);
        }
        return Err(format!("Windows Credential Manager could not read the Terminal device credential: {}", std::io::Error::from_raw_os_error(error as i32)));
    }
    if credential.is_null() {
        return Err("Windows Credential Manager returned an empty Terminal credential".to_owned());
    }

    // SAFETY: CredReadW returned a live credential allocation, which remains valid until CredFree below.
    let copied = unsafe {
        let value = &*credential;
        if value.CredentialBlob.is_null() || value.CredentialBlobSize as usize > MAX_CREDENTIAL_BYTES || value.UserName.is_null() {
            Err("Saved Terminal device credential is empty or invalid".to_owned())
        } else {
            let bytes = Zeroizing::new(std::slice::from_raw_parts(value.CredentialBlob, value.CredentialBlobSize as usize).to_vec());
            let mut username_len = 0usize;
            while *value.UserName.add(username_len) != 0 {
                username_len += 1;
                if username_len > 513 {
                    break;
                }
            }
            if username_len > 513 {
                Err("Saved Terminal credential metadata is invalid".to_owned())
            } else {
                let username = std::ffi::OsString::from_wide(std::slice::from_raw_parts(value.UserName, username_len))
                    .to_string_lossy()
                    .into_owned();
    Ok((username, bytes))
            }
        }
    };
    // SAFETY: credential is the allocation returned by CredReadW and is freed exactly once.
    unsafe { CredFree(credential.cast()) };
    copied.map(Some)
}

#[cfg(not(windows))]
fn read_windows(_target: &str) -> Result<Option<(String, Zeroizing<Vec<u8>>)>, String> {
    Err("Terminal device credentials are available only on Windows".to_owned())
}

#[cfg(windows)]
fn delete_windows(target: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW},
    };

    let mut target_wide: Vec<u16> = std::ffi::OsStr::new(target).encode_wide().collect();
    target_wide.push(0);
    // SAFETY: target_wide is NUL-terminated and remains alive for this call.
    let deleted = unsafe { CredDeleteW(target_wide.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if deleted != 0 {
        return Ok(());
    }
    let error = unsafe { GetLastError() };
    if error == 1168 {
        Ok(())
    } else {
        Err(format!("Windows Credential Manager could not remove the Terminal device credential: {}", std::io::Error::from_raw_os_error(error as i32)))
    }
}

#[cfg(not(windows))]
fn delete_windows(_target: &str) -> Result<(), String> {
    Err("Terminal device credentials are available only on Windows".to_owned())
}
