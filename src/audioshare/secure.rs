//! Token storage.
//!
//! * Personal mode: the token is kept in Windows Credential Manager.
//! * Shared studio mode (spec §4.4): the token is encrypted with a key derived
//!   from the maker's PIN (Argon2id + AES-256-GCM). Without the PIN the stored
//!   token is useless, even for someone using the same Windows account.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sealed {
    pub salt: String,
    pub nonce: String,
    pub data: String,
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("random");
    b
}

fn derive_key(pin: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    use argon2::{Algorithm, Argon2, Params, Version};
    // 64 MiB, 3 passes: about half a second on a studio PC.
    let params = Params::new(64 * 1024, 3, 1, Some(32)).map_err(|e| e.to_string())?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pin.as_bytes(), salt, &mut key)
        .map_err(|e| e.to_string())?;
    Ok(key)
}

pub fn seal(secret: &str, pin: &str) -> Result<Sealed, String> {
    let salt = random::<16>();
    let nonce = random::<12>();
    let key = derive_key(pin, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let data = cipher.encrypt(Nonce::from_slice(&nonce), secret.as_bytes()).map_err(|_| "versleutelen mislukt".to_string())?;
    Ok(Sealed { salt: B64.encode(salt), nonce: B64.encode(nonce), data: B64.encode(data) })
}

/// Returns None when the PIN is wrong.
pub fn open(sealed: &Sealed, pin: &str) -> Option<String> {
    let salt = B64.decode(&sealed.salt).ok()?;
    let nonce = B64.decode(&sealed.nonce).ok()?;
    let data = B64.decode(&sealed.data).ok()?;
    if nonce.len() != 12 {
        return None;
    }
    let key = derive_key(pin, &salt).ok()?;
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let plain = cipher.decrypt(Nonce::from_slice(&nonce), data.as_ref()).ok()?;
    String::from_utf8(plain).ok()
}

// ---------------------------------------------------------------------------
// Windows Credential Manager
// ---------------------------------------------------------------------------

fn target(profile_id: &str) -> String {
    format!("StudioRecorder/AudioShare/{profile_id}")
}

#[cfg(windows)]
mod cred {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn write(target: &str, secret: &str) -> Result<(), String> {
        let mut t = wide(target);
        let mut user = wide("StudioRecorder");
        let blob = secret.as_bytes().to_vec();
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: t.as_mut_ptr(),
            Comment: std::ptr::null_mut(),
            LastWritten: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user.as_mut_ptr(),
        };
        let ok = unsafe { CredWriteW(&cred, 0) };
        if ok != 0 { Ok(()) } else { Err("Kan token niet opslaan in Windows Credential Manager".into()) }
    }

    pub fn read(target: &str) -> Option<String> {
        let t = wide(target);
        let mut p: *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe { CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) };
        if ok == 0 || p.is_null() {
            return None;
        }
        let s = unsafe {
            let c = &*p;
            let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            String::from_utf8(bytes.to_vec()).ok()
        };
        unsafe { CredFree(p as *const _) };
        s
    }

    pub fn delete(target: &str) {
        let t = wide(target);
        unsafe {
            CredDeleteW(t.as_ptr(), CRED_TYPE_GENERIC, 0);
        }
    }
}

/// Development fallback on other systems: a file in the config folder.
#[cfg(not(windows))]
mod cred {
    fn path(target: &str) -> std::path::PathBuf {
        crate::settings::config_dir().join(format!("cred_{}.txt", target.replace('/', "_")))
    }
    pub fn write(target: &str, secret: &str) -> Result<(), String> {
        let p = path(target);
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::write(p, secret).map_err(|e| e.to_string())
    }
    pub fn read(target: &str) -> Option<String> {
        std::fs::read_to_string(path(target)).ok()
    }
    pub fn delete(target: &str) {
        let _ = std::fs::remove_file(path(target));
    }
}

pub fn store_token(profile_id: &str, token: &str) -> Result<(), String> {
    cred::write(&target(profile_id), token)
}

pub fn load_token(profile_id: &str) -> Option<String> {
    cred::read(&target(profile_id))
}

pub fn delete_token(profile_id: &str) {
    cred::delete(&target(profile_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_open() {
        let s = seal("as_mk_secret", "123456").unwrap();
        assert_eq!(open(&s, "123456").as_deref(), Some("as_mk_secret"));
        assert_eq!(open(&s, "123457"), None);
        assert!(!s.data.contains("secret"));
    }
}
