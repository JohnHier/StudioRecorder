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
    /// "studio": this PC is linked as a studio (AudioShare 1.5.0). The token
    /// then lives in Windows Credential Manager and people sign in as local users.
    pub account_type: String,
}

impl Profile {
    pub fn is_admin(&self) -> bool {
        self.role.eq_ignore_ascii_case("admin")
    }
    pub fn is_studio(&self) -> bool {
        self.account_type.eq_ignore_ascii_case("studio")
    }
}

/// A person who works in a linked studio. Exists only on this PC; the studio
/// manager creates them and picks their programmes from the studio's list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalUser {
    pub id: String,
    pub name: String,
    /// The PIN, as a sealed check value (never stored readable).
    pub pin: Option<Sealed>,
    /// Programme ids from the studio's list. Ignored for a manager (all).
    pub programs: Vec<i64>,
    /// Studio manager: settings, users, all programmes, all uploads.
    pub manager: bool,
    pub pin_fails: u32,
    pub locked_until: i64,
}

const PIN_CHECK: &str = "studiorecorder-local-user";

pub enum LocalUnlock {
    Ok,
    Wrong { attempts_left: u32 },
    Locked { seconds: i64 },
}

pub fn valid_pin(pin: &str) -> Result<(), String> {
    if pin.len() < 4 || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err("De pincode moet uit minimaal 4 cijfers bestaan.".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// "Gedeelde studio": several makers on this PC, each with a PIN.
    pub shared: bool,
    pub auto_lock_minutes: u32,
    pub profiles: Vec<Profile>,
    /// Studio mode: the people who work here (see LocalUser).
    pub local_users: Vec<LocalUser>,
}

impl Default for Store {
    fn default() -> Self {
        Store { shared: true, auto_lock_minutes: 15, profiles: Vec::new(), local_users: Vec::new() }
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

    // -----------------------------------------------------------------------
    // Studio mode
    // -----------------------------------------------------------------------

    /// The studio link of this PC, if it is linked as a studio.
    pub fn studio(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.is_studio())
    }

    /// Store a studio link. The token goes to Windows Credential Manager, so
    /// queued uploads continue without anyone signed in. An earlier studio
    /// link is replaced; local users are kept. Without local users the first
    /// one becomes the studio manager. Returns (profile id, manager id if new).
    pub fn add_studio(&mut self, p: Profile, token: &str, manager_name: &str, pin: &str) -> Result<(String, Option<String>), String> {
        let new_manager = if self.local_users.iter().any(|u| u.manager) {
            None
        } else {
            valid_pin(pin)?;
            let name = if manager_name.trim().is_empty() { "Studiobeheerder" } else { manager_name.trim() };
            Some(self.new_local_user(name, pin, Vec::new(), true)?)
        };
        secure::store_token(&p.id, token)?;
        let old: Vec<String> = self.profiles.iter().filter(|x| x.is_studio()).map(|x| x.id.clone()).collect();
        for id in old {
            secure::delete_token(&id);
            self.profiles.retain(|x| x.id != id);
        }
        let id = p.id.clone();
        self.profiles.push(p);
        if let Some(u) = &new_manager {
            self.local_users.push(u.clone());
        }
        self.sort_users();
        self.save();
        Ok((id, new_manager.map(|u| u.id)))
    }

    fn new_local_user(&self, name: &str, pin: &str, programs: Vec<i64>, manager: bool) -> Result<LocalUser, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Vul een naam in.".into());
        }
        if self.local_users.iter().any(|u| u.name.eq_ignore_ascii_case(name)) {
            return Err(format!("Er is al een gebruiker {name}."));
        }
        valid_pin(pin)?;
        Ok(LocalUser {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            pin: Some(secure::seal(PIN_CHECK, pin)?),
            programs,
            manager,
            ..Default::default()
        })
    }

    fn sort_users(&mut self) {
        self.local_users.sort_by_key(|u| (!u.manager, u.name.to_lowercase()));
    }

    pub fn local_user(&self, id: &str) -> Option<&LocalUser> {
        self.local_users.iter().find(|u| u.id == id)
    }

    pub fn add_local_user(&mut self, name: &str, pin: &str, programs: Vec<i64>, manager: bool) -> Result<String, String> {
        let u = self.new_local_user(name, pin, programs, manager)?;
        let id = u.id.clone();
        self.local_users.push(u);
        self.sort_users();
        self.save();
        Ok(id)
    }

    /// Change a user's name, programmes and manager flag (the last manager stays one).
    pub fn update_local_user(&mut self, id: &str, name: &str, programs: Vec<i64>, manager: bool) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Vul een naam in.".into());
        }
        if self.local_users.iter().any(|u| u.id != id && u.name.eq_ignore_ascii_case(name)) {
            return Err(format!("Er is al een gebruiker {name}."));
        }
        let others_manage = self.local_users.iter().any(|u| u.id != id && u.manager);
        let u = self.local_users.iter_mut().find(|u| u.id == id).ok_or("Gebruiker niet gevonden")?;
        if u.manager && !manager && !others_manage {
            return Err("Er moet minstens één studiobeheerder zijn.".into());
        }
        u.name = name.to_string();
        u.programs = programs;
        u.manager = manager;
        self.sort_users();
        self.save();
        Ok(())
    }

    /// New PIN for a user (by the manager, or by the user after a lockout); unblocks.
    pub fn set_local_pin(&mut self, id: &str, pin: &str) -> Result<(), String> {
        valid_pin(pin)?;
        let sealed = secure::seal(PIN_CHECK, pin)?;
        let u = self.local_users.iter_mut().find(|u| u.id == id).ok_or("Gebruiker niet gevonden")?;
        u.pin = Some(sealed);
        u.pin_fails = 0;
        u.locked_until = 0;
        self.save();
        Ok(())
    }

    pub fn remove_local_user(&mut self, id: &str) -> Result<(), String> {
        let u = self.local_user(id).ok_or("Gebruiker niet gevonden")?;
        if u.manager && !self.local_users.iter().any(|x| x.id != id && x.manager) {
            return Err("De laatste studiobeheerder kan niet worden verwijderd.".into());
        }
        self.local_users.retain(|u| u.id != id);
        self.save();
        Ok(())
    }

    /// Check a local user's PIN: 5 wrong in a row = 5 minutes locked (again and again).
    pub fn unlock_local(&mut self, id: &str, pin: &str) -> LocalUnlock {
        let t = now();
        let Some(u) = self.local_users.iter_mut().find(|u| u.id == id) else { return LocalUnlock::Wrong { attempts_left: 0 } };
        if u.locked_until > t {
            return LocalUnlock::Locked { seconds: u.locked_until - t };
        }
        let ok = u.pin.as_ref().and_then(|s| secure::open(s, pin)).as_deref() == Some(PIN_CHECK);
        let res = if ok {
            u.pin_fails = 0;
            u.locked_until = 0;
            LocalUnlock::Ok
        } else {
            u.pin_fails += 1;
            if u.pin_fails % 5 == 0 {
                u.locked_until = t + 300;
                LocalUnlock::Locked { seconds: 300 }
            } else {
                LocalUnlock::Wrong { attempts_left: 5 - u.pin_fails % 5 }
            }
        };
        self.save();
        res
    }

    /// The programmes a local user may upload: the studio's list, narrowed to
    /// the user's selection (a manager gets them all).
    pub fn programs_for(&self, user_id: &str) -> Vec<super::api::Program> {
        let (Some(st), Some(u)) = (self.studio(), self.local_user(user_id)) else { return Vec::new() };
        st.programs.iter().filter(|p| u.manager || u.programs.contains(&p.id)).cloned().collect()
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

    #[test]
    fn studio_users_and_programmes() {
        let mut s = Store::default();
        let progs = |ids: &[i64]| ids.iter().map(|&id| crate::audioshare::api::Program { id, name: format!("P{id}"), ..Default::default() }).collect::<Vec<_>>();
        s.profiles.push(Profile { id: "st".into(), display_name: "Studio 2".into(), account_type: "studio".into(), programs: progs(&[1, 2, 3]), ..Default::default() });
        let m = s.new_local_user("Anne", "1234", Vec::new(), true).unwrap();
        let k = s.new_local_user("Kees", "5678", vec![2, 9], false).unwrap();
        s.local_users.push(m.clone());
        s.local_users.push(k.clone());
        assert!(s.studio().is_some());
        // manager: all studio programmes; Kees: only 2 (9 is not in the studio's list)
        assert_eq!(s.programs_for(&m.id).iter().map(|p| p.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(s.programs_for(&k.id).iter().map(|p| p.id).collect::<Vec<_>>(), vec![2]);
        assert!(s.new_local_user("kees", "1111", Vec::new(), false).is_err());
        assert!(s.new_local_user("Piet", "12", Vec::new(), false).is_err());
        // PIN check with lockout, no save to the real file needed for the check itself
        let id = k.id.clone();
        let check = |s: &mut Store, pin: &str| {
            let t = now();
            let u = s.local_users.iter_mut().find(|u| u.id == id).unwrap();
            if u.locked_until > t { return "locked"; }
            if u.pin.as_ref().and_then(|x| secure::open(x, pin)).as_deref() == Some(PIN_CHECK) { "ok" } else { "wrong" }
        };
        assert_eq!(check(&mut s, "5678"), "ok");
        assert_eq!(check(&mut s, "0000"), "wrong");
        // the last manager can't lose the role
        let others_manage = s.local_users.iter().any(|u| u.id != m.id && u.manager);
        assert!(!others_manage);
    }
}
