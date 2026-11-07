use uuid::Uuid;
use zeroize::Zeroizing;

const TARGET_PREFIX: &str = "ShockingVRC.Terminal/host/";
const MAX_CREDENTIAL_BYTES: usize = 5_120;
const RENDEZVOUS_PREFIX: &str = "ShockingVRC.Terminal/rendezvous/";

pub fn has_saved(profile_id: &str, fingerprint: &str) -> Result<bool, String> {
    Ok(read(profile_id, fingerprint)?.is_some())
}

pub fn save(profile_id: &str, fingerprint: &str, username: &str, password: &str) -> Result<(), String> {
    let target = target_name(profile_id)?;
    if username.is_empty() || username.chars().count() > 128 || password.is_empty() || fingerprint.len() != 64 {
        return Err("The Host credentials are not valid for Windows Credential Manager".to_owned());
    }
    let mut blob = Zeroizing::new(Vec::with_capacity(65 + password.len()));
    blob.extend_from_slice(fingerprint.as_bytes());
    blob.push(0);
    blob.extend_from_slice(password.as_bytes());
    if blob.len() > MAX_CREDENTIAL_BYTES {
        return Err("The saved Host password exceeds the Windows Credential Manager limit".to_owned());
    }
    write_windows(&target, username, &blob)
}

pub fn read(profile_id: &str, fingerprint: &str) -> Result<Option<(String, Zeroizing<String>)>, String> {
    let target = target_name(profile_id)?;
    let Some((username, blob)) = read_windows(&target)? else {
        return Ok(None);
    };
    if fingerprint.len() != 64 || blob.len() < 66 || blob.get(64) != Some(&0) || &blob[..64] != fingerprint.as_bytes() {
        return Ok(None);
    }
    let password = std::str::from_utf8(&blob[65..])
        .map_err(|_| "Saved Host password is invalid".to_owned())?;
    if password.is_empty() {
        return Ok(None);
    }
    Ok(Some((username, Zeroizing::new(password.to_owned()))))
}

pub fn delete(profile_id: &str) -> Result<(), String> {
    let target = target_name(profile_id)?;
    delete_windows(&target)?;
    delete_windows(&invite_target_name(profile_id)?)
}

pub fn save_invite_token(profile_id: &str, fingerprint: &str, token: &str) -> Result<(), String> {
    let target = invite_target_name(profile_id)?;
    if fingerprint.len() != 64 || token.trim().is_empty() || token.len() > 4_096 {
        return Err("The rendezvous invite token is invalid".to_owned());
    }
    let mut blob = Zeroizing::new(Vec::with_capacity(65 + token.len()));
    blob.extend_from_slice(fingerprint.as_bytes());
    blob.push(0);
    blob.extend_from_slice(token.trim().as_bytes());
    write_windows(&target, "rendezvous", &blob)
}

pub fn has_invite_token(profile_id: &str, fingerprint: &str) -> Result<bool, String> {
    Ok(read_invite_token(profile_id, fingerprint)?.is_some())
}

pub fn delete_invite_token(profile_id: &str) -> Result<(), String> {
    delete_windows(&invite_target_name(profile_id)?)
}

pub fn read_invite_token(profile_id: &str, fingerprint: &str) -> Result<Option<Zeroizing<String>>, String> {
    let Some((_, blob)) = read_windows(&invite_target_name(profile_id)?)? else { return Ok(None); };
    if fingerprint.len() != 64 || blob.len() < 66 || blob.get(64) != Some(&0) || &blob[..64] != fingerprint.as_bytes() {
        return Ok(None);
    }
    let token = std::str::from_utf8(&blob[65..]).map_err(|_| "Saved rendezvous invite token is invalid".to_owned())?;
    if token.is_empty() { return Ok(None); }
    Ok(Some(Zeroizing::new(token.to_owned())))
}

fn invite_target_name(profile_id: &str) -> Result<String, String> {
    let id = Uuid::parse_str(profile_id).map_err(|_| "Host profile ID is invalid".to_owned())?;
    Ok(format!("{RENDEZVOUS_PREFIX}{id}"))
}

fn target_name(profile_id: &str) -> Result<String, String> {
    let id = Uuid::parse_str(profile_id).map_err(|_| "Host profile ID is invalid".to_owned())?;
    Ok(format!("{TARGET_PREFIX}{id}"))
}

#[cfg(windows)]
fn write_windows(target: &str, username: &str, password: &[u8]) -> Result<(), String> {
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
        CredentialBlobSize: u32::try_from(password.len()).map_err(|_| "Saved Host password is too large".to_owned())?,
        CredentialBlob: password.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username_wide.as_mut_ptr(),
        ..Default::default()
    };
    // SAFETY: all pointers reference NUL-terminated buffers or a byte slice that remain alive for this call.
    let written = unsafe { CredWriteW(&credential, 0) };
    if written == 0 {
        Err(format!("Windows Credential Manager could not save the Host credential: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn write_windows(_target: &str, _username: &str, _password: &[u8]) -> Result<(), String> {
    Err("Saved Host credentials are available only on Windows".to_owned())
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
        return Err(format!("Windows Credential Manager could not read the Host credential: {}", std::io::Error::from_raw_os_error(error as i32)));
    }
    if credential.is_null() {
        return Err("Windows Credential Manager returned an empty credential".to_owned());
    }

    // SAFETY: CredReadW returned a live credential allocation, which remains valid until CredFree below.
    let copied = unsafe {
        let value = &*credential;
        if value.CredentialBlob.is_null() || value.CredentialBlobSize as usize > MAX_CREDENTIAL_BYTES || value.UserName.is_null() {
            Err("Saved Host credential is empty or invalid".to_owned())
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
                Err("Saved Host username is invalid".to_owned())
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
    Err("Saved Host credentials are available only on Windows".to_owned())
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
        Err(format!("Windows Credential Manager could not remove the Host credential: {}", std::io::Error::from_raw_os_error(error as i32)))
    }
}

#[cfg(not(windows))]
fn delete_windows(_target: &str) -> Result<(), String> {
    Err("Saved Host credentials are available only on Windows".to_owned())
}
