//! Linked AudioShare accounts ("profielen") on this computer.
//!
//! Stored in `%APPDATA%\StudioRecorder\audioshare.json`. That file never holds
//! a readable token: in personal mode the token lives in Windows Credential
//! Manager, in shared studio mode it is sealed with the maker's PIN.

use super::api::Program;
use super::secure::{self, Sealed};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Local id (uuid), also used for the Credential Manager entry.
    pub id: String,
    pub display_name: String,
    pub user_id: i64,
    pub device_name: String,
    pub url: String,
    pub linked_at: String,
    /// Shared studio mode: token sealed with the PIN.
    pub sealed: Option<Sealed>,
    pub pin_fails: u32,
    /// Unix seconds until which PIN entry is blocked.
    pub locked_until: i64,
    /// Set after a 401 or 10 wrong PINs: the maker must pair again.
    pub needs_relink: bool,
    /// Last known programme list (with quality norms).
    pub programs: Vec<Program>,
    /// Role in AudioShare ("admin" may change the recorder's settings).
    pub role: String,
}

impl Profile {
    pub fn is_admin(&self) -> bool {
        self.role.eq_ignore_ascii_case("admin")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// "Gedeelde studio": several makers on this PC, each with a PIN.
    pub shared: bool,
    pub auto_lock_minutes: u32,
    pub profiles: Vec<Profile>,
}

impl Default for Store {
    fn default() -> Self {
        Store { shared: true, auto_lock_minutes: 15, profiles: Vec::new() }
    }
}

pub enum Unlock {
    Ok(String),
    Wrong { attempts_left: u32 },
    Locked { seconds: i64 },
    Relink,
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn path() -> PathBuf {
    crate::settings::config_dir().join("audioshare.json")
}

impl Store {
    pub fn load() -> Store {
        let mut s: Store = std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        // Since 0.6.1 everyone always logs in with a PIN; older "personal"
        // links get a PIN the first time they log in (see set_pin).
        s.shared = true;
        s
    }

    /// A link from before 0.6.1 without a PIN: token still in Credential Manager.
    pub fn needs_pin(&self, id: &str) -> bool {
        self.get(id).is_some_and(|p| p.sealed.is_none() && !p.needs_relink) && secure::load_token(id).is_some()
    }

    /// Give an old link a PIN: seal its token and remove it from Credential Manager.
    pub fn set_pin(&mut self, id: &str, pin: &str) -> Result<String, String> {
        let token = secure::load_token(id).ok_or("Deze koppeling is niet meer geldig; koppel opnieuw.")?;
        let sealed = secure::seal(&token, pin)?;
        let p = self.get_mut(id).ok_or("Profiel niet gevonden")?;
        p.sealed = Some(sealed);
        p.pin_fails = 0;
        p.locked_until = 0;
        secure::delete_token(id);
        self.save();
        Ok(token)
    }

    pub fn save(&self) {
        crate::settings::write_json(&path(), self);
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    /// Store a freshly paired account. In personal mode it replaces any
    /// earlier link; in shared mode the same AudioShare user is replaced.
    pub fn add(&mut self, mut p: Profile, token: &str, pin: Option<&str>) -> Result<String, String> {
        if self.shared {
            let pin = pin.ok_or("Kies een pincode")?;
            p.sealed = Some(secure::seal(token, pin)?);
        } else {
            secure::store_token(&p.id, token)?;
        }
        let same: Vec<String> = self
            .profiles
            .iter()
            .filter(|x| !self.shared || (x.user_id == p.user_id && x.url == p.url))
            .map(|x| x.id.clone())
            .collect();
        for id in same {
            self.remove(&id);
        }
        let id = p.id.clone();
        self.profiles.push(p);
        self.profiles.sort_by_key(|a| a.display_name.to_lowercase());
        self.save();
        Ok(id)
    }

    /// Forget a profile and its token on this computer.
    pub fn remove(&mut self, id: &str) {
        secure::delete_token(id);
        self.profiles.retain(|p| p.id != id);
        self.save();
    }

    /// The token was rejected by AudioShare (401): wipe it, keep the name so
    /// the maker sees what happened.
    pub fn mark_unlinked(&mut self, id: &str) {
        secure::delete_token(id);
        if let Some(p) = self.get_mut(id) {
            p.sealed = None;
            p.needs_relink = true;
        }
        self.save();
    }

    /// Shared mode: check the PIN, with 5-minute lockout after 5 wrong tries
    /// and a forced re-pairing after 10 (spec §4.4).
    pub fn unlock(&mut self, id: &str, pin: &str) -> Unlock {
        let t = now();
        let Some(p) = self.get_mut(id) else { return Unlock::Relink };
        if p.needs_relink || p.sealed.is_none() {
            // (an old link without PIN is handled by set_pin before this)
            return Unlock::Relink;
        }
        if p.locked_until > t {
            return Unlock::Locked { seconds: p.locked_until - t };
        }
        let res = match secure::open(p.sealed.as_ref().unwrap(), pin) {
            Some(token) => {
                p.pin_fails = 0;
                p.locked_until = 0;
                Unlock::Ok(token)
            }
            None => {
                p.pin_fails += 1;
                if p.pin_fails >= 10 {
                    p.sealed = None;
                    p.needs_relink = true;
                    Unlock::Relink
                } else if p.pin_fails % 5 == 0 {
                    p.locked_until = t + 300;
                    Unlock::Locked { seconds: 300 }
                } else {
                    Unlock::Wrong { attempts_left: 5 - p.pin_fails % 5 }
                }
            }
        };
        self.save();
        res
    }
}

/// The Windows computer name, used as the default device name.
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "STUDIO".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_lockout_and_relink() {
        let mut s = Store { shared: true, ..Default::default() };
        let p = Profile { id: "t1".into(), display_name: "Pietje".into(), user_id: 13, ..Default::default() };
        // keep the test away from the real profile file: no save via add()
        let mut p2 = p.clone();
        p2.sealed = Some(secure::seal("as_mk_tok", "4321").unwrap());
        s.profiles.push(p2);
        assert!(matches!(s.unlock("t1", "4321"), Unlock::Ok(t) if t == "as_mk_tok"));
        for left in [4, 3, 2, 1] {
            assert!(matches!(s.unlock("t1", "0000"), Unlock::Wrong { attempts_left } if attempts_left == left));
        }
        assert!(matches!(s.unlock("t1", "0000"), Unlock::Locked { seconds: 300 }));
        // even the right PIN is refused while locked
        assert!(matches!(s.unlock("t1", "4321"), Unlock::Locked { .. }));
        s.get_mut("t1").unwrap().locked_until = 0;
        for _ in 0..4 {
            assert!(matches!(s.unlock("t1", "1111"), Unlock::Wrong { .. }));
        }
        assert!(matches!(s.unlock("t1", "1111"), Unlock::Relink));
        assert!(s.get("t1").unwrap().sealed.is_none());
        assert!(matches!(s.unlock("t1", "4321"), Unlock::Relink));
    }
}
