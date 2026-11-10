//! AudioShare in the interface: pairing, signing in (shared studio), the
//! upload queue screen, the replace dialog and the settings section.

use super::*;
use crate::audioshare::api::{self, ApiError, Client, Program, UploadInfo};
use crate::audioshare::profiles::{self, LocalUnlock, Profile, Store, Unlock, computer_name};
use crate::audioshare::queue::{Job, JobState, Level, Queue, mmss};
use crossbeam_channel::{Sender, unbounded};
use std::collections::HashSet;

pub(super) enum Msg {
    /// (profile, token, problem fetching the programme list)
    Claimed(Result<(Profile, String, Option<String>), String>),
    Programs(String, Result<Vec<Program>, ApiError>),
    Role(String, String),
    /// (programme id, for the editor?, result)
    Info(i64, bool, Result<UploadInfo, ApiError>),
    Tested(String, Result<String, ApiError>),
    /// A newer Studio Recorder is available in AudioShare.
    NewVersion(String),
}

#[derive(Default)]
pub(super) struct LinkForm {
    url: String,
    code: String,
    device: String,
    pin: String,
    pin2: String,
    from_file: Option<String>,
    /// The `.audioshare` file this form came from; retired after pairing.
    file_path: Option<std::path::PathBuf>,
    /// The file says this is a studio link (AudioShare → Recorders → Studio's).
    studio: bool,
    /// Studio link: name of the studio manager (first local user).
    manager_name: String,
    busy: bool,
    error: Option<String>,
    done: Option<String>,
}

#[derive(Default)]
pub(super) struct UnlockForm {
    selected: Option<String>,
    pin: String,
    pin2: String,
    msg: Option<String>,
    focus: bool,
}

pub(super) struct AsUi {
    pub store: Store,
    pub queue: Arc<Queue>,
    /// Signed-in (shared) or linked (personal) profile.
    pub active: Option<String>,
    last_activity: Instant,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub link: Option<LinkForm>,
    pub unlock: Option<UnlockForm>,
    confirm_job: Option<String>,
    shown_confirm: HashSet<String>,
    test_msg: Option<(String, bool)>,
    /// Messages for the toast line, collected during housekeeping.
    pub notices: Vec<(String, bool)>,
    update_notified: bool,
    /// Studio mode: the signed-in local user.
    local_user: Option<String>,
    /// Studio mode: the studio link as the signed-in local user sees it
    /// (their name, their programmes, manager = "admin").
    studio_view: Option<Profile>,
    /// Studio mode: the users dialog in the settings.
    pub users_form: Option<UsersForm>,
}

#[derive(Default)]
pub(super) struct UsersForm {
    /// None = new user.
    editing: Option<String>,
    name: String,
    pin: String,
    pin2: String,
    manager: bool,
    programs: HashSet<i64>,
    msg: Option<(String, bool)>,
}

impl AsUi {
    pub fn new() -> AsUi {
        let (tx, rx) = unbounded();
        let a = AsUi {
            store: Store::load(),
            queue: Queue::start(),
            active: None,
            last_activity: Instant::now(),
            tx,
            rx,
            link: None,
            unlock: None,
            confirm_job: None,
            shown_confirm: HashSet::new(),
            test_msg: None,
            notices: Vec::new(),
            update_notified: false,
            local_user: None,
            studio_view: None,
            users_form: None,
        };
        // Always start logged out. A studio's token is always available (Credential
        // Manager), so its queued uploads carry on without anyone signed in.
        if let Some(st) = a.store.studio() {
            if let Some(tok) = crate::audioshare::secure::load_token(&st.id) {
                a.queue.tokens.lock().unwrap().insert(st.id.clone(), (st.url.clone(), tok));
            }
        }
        a
    }

    /// This PC is linked as a studio: people sign in as local users.
    pub fn is_studio(&self) -> bool {
        self.store.studio().is_some()
    }

    fn studio_id(&self) -> Option<String> {
        self.store.studio().map(|p| p.id.clone())
    }

    /// Who operated, sent with each upload from a studio.
    pub fn operator(&self) -> String {
        match (self.is_studio(), &self.local_user) {
            (true, Some(id)) => self.store.local_user(id).map(|u| u.name.clone()).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Changes when another person signs in (also between local users of a studio).
    pub fn session_key(&self) -> Option<String> {
        self.active.as_ref().map(|a| format!("{a}/{}", self.local_user.clone().unwrap_or_default()))
    }

    fn rebuild_view(&mut self) {
        self.studio_view = match (self.store.studio(), &self.local_user) {
            (Some(st), Some(uid)) => self.store.local_user(uid).map(|u| {
                let mut v = st.clone();
                v.display_name = u.name.clone();
                v.role = if u.manager { "admin".into() } else { String::new() };
                v.programs = self.store.programs_for(uid);
                v
            }),
            _ => None,
        };
    }

    fn studio_sign_in(&mut self, user_id: &str) {
        let Some(st) = self.store.studio().cloned() else { return };
        let token = self.queue.tokens.lock().unwrap().get(&st.id).map(|t| t.1.clone()).or_else(|| crate::audioshare::secure::load_token(&st.id));
        let Some(token) = token else {
            self.notices.push(("De studiokoppeling is niet meer geldig. Koppel de studio opnieuw met een nieuwe code van de admin.".into(), true));
            return;
        };
        self.local_user = Some(user_id.to_string());
        self.activate(&st.id, token);
        self.rebuild_view();
    }

    /// Is there an admin profile on this PC? (A studio always has a manager.)
    pub fn has_admin(&self) -> bool {
        self.is_studio() || self.store.profiles.iter().any(|p| p.is_admin())
    }

    /// May the current user change settings? Yes for an admin, and for
    /// everyone as long as no admin has been linked on this PC (first setup).
    pub fn admin_unlocked(&self) -> bool {
        !self.has_admin() || self.active_profile().is_some_and(|p| p.is_admin())
    }

    pub fn is_admin(&self) -> bool {
        self.active_profile().is_some_and(|p| p.is_admin())
    }

    pub fn configured(&self) -> bool {
        !self.store.profiles.is_empty()
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        if self.is_studio() {
            return self.active.as_ref().and(self.studio_view.as_ref());
        }
        self.active.as_ref().and_then(|id| self.store.get(id))
    }

    pub fn active_name(&self) -> Option<String> {
        self.active_profile().map(|p| p.display_name.clone())
    }

    pub fn program_source(&self) -> ProgramSource<'_> {
        if !self.configured() {
            return ProgramSource::Local;
        }
        match self.active_profile() {
            Some(p) => ProgramSource::Remote(&p.programs),
            None => ProgramSource::Locked,
        }
    }

    pub fn program(&self, id: i64) -> Option<&Program> {
        self.active_profile().and_then(|p| p.programs.iter().find(|x| x.id == id))
    }

    fn client(&self) -> Option<Client> {
        let id = self.active.as_ref()?;
        let (url, token) = self.queue.tokens.lock().unwrap().get(id).cloned()?;
        Some(Client::new(&url, Some(token)))
    }

    fn activate(&mut self, id: &str, token: String) {
        if let Some(prev) = self.active.take() {
            if prev != id {
                self.release_token(&prev);
            }
        }
        let Some(p) = self.store.get(id) else { return };
        self.queue.tokens.lock().unwrap().insert(id.to_string(), (p.url.clone(), token));
        self.active = Some(id.to_string());
        self.last_activity = Instant::now();
        self.queue.wake();
        self.refresh_programs();
        self.check_update();
    }

    /// Drop a token from memory, unless its uploads are still running (§4.4).
    /// A studio's token stays: it is not tied to a person.
    fn release_token(&self, id: &str) {
        if self.store.get(id).is_some_and(|p| p.is_studio()) {
            return;
        }
        if !self.queue.has_open_jobs(id) {
            self.queue.tokens.lock().unwrap().remove(id);
        }
    }

    pub fn sign_out(&mut self) {
        if let Some(id) = self.active.take() {
            self.release_token(&id);
        }
        self.local_user = None;
        self.studio_view = None;
        self.users_form = None;
        self.confirm_job = None;
    }

    pub fn refresh_programs(&self) {
        let (Some(c), Some(id)) = (self.client(), self.active.clone()) else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Ok(me) = c.me() {
                let _ = tx.send(Msg::Role(id.clone(), me.user.role));
            }
            let _ = tx.send(Msg::Programs(id, c.programs()));
        });
    }

    pub fn fetch_info(&self, program_id: i64, for_editor: bool) {
        let Some(c) = self.client() else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Info(program_id, for_editor, c.upload_info(program_id)));
        });
    }

    fn handle_api_error(&mut self, profile_id: &str, e: &ApiError) {
        match e.code() {
            "INVALID_TOKEN" => self.unlinked(profile_id),
            "NOT_ASSIGNED" => self.refresh_programs(),
            _ => self.notices.push((e.user_message(), true)),
        }
    }

    fn unlinked(&mut self, id: &str) {
        let studio = self.store.get(id).is_some_and(|p| p.is_studio());
        self.store.mark_unlinked(id);
        self.queue.tokens.lock().unwrap().remove(id);
        if self.active.as_deref() == Some(id) {
            self.sign_out();
        }
        let msg = if studio {
            "Deze studio is in AudioShare ontkoppeld — de admin maakt een nieuwe koppelcode (Recorders → Studio's)."
        } else {
            "Deze recorder is ontkoppeld — vraag je beheerder om een nieuwe koppelcode."
        };
        self.notices.push((msg.into(), true));
    }

    pub fn open_link(&mut self, file: Option<&Path>) {
        let url = self.store.profiles.first().map(|p| p.url.clone()).unwrap_or_else(|| "https://platform.br6.nl".into());
        let mut f = LinkForm { device: computer_name(), url, ..Default::default() };
        if let Some(p) = file {
            match api::read_link_file(p) {
                Ok(l) => {
                    f.url = l.url;
                    f.code = l.claim_code;
                    let mut src = if l.user.is_empty() { "koppelbestand".to_string() } else { format!("koppelbestand van {}", l.user) };
                    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&l.expires) {
                        if t < chrono::Local::now() {
                            f.error = Some(format!(
                                "Deze koppelcode is verlopen op {}. Vraag je beheerder om een nieuwe.",
                                t.format("%d-%m-%Y %H:%M")
                            ));
                        } else {
                            src.push_str(&format!(" (geldig tot {})", t.format("%d-%m %H:%M")));
                        }
                    }
                    f.from_file = Some(src);
                    f.file_path = Some(p.to_path_buf());
                    f.studio = l.link_kind.eq_ignore_ascii_case("studio");
                }
                Err(e) => f.error = Some(e),
            }
        }
        self.unlock = None;
        self.link = Some(f);
    }

    /// First start from the zip of AudioShare → Recorders: a personal `.audioshare`
    /// file lies next to the program. Open the pairing form with it straight away.
    pub fn auto_link(&mut self) {
        if let Some(p) = api::link_files_next_to_exe().into_iter().next() {
            self.open_link(Some(&p));
        }
    }

    fn check_update(&self) {
        let Some(c) = self.client() else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Some(v) = c.latest_recorder() {
                if api::newer_version(&v, env!("CARGO_PKG_VERSION")) {
                    let _ = tx.send(Msg::NewVersion(v));
                }
            }
        });
    }

    /// Called every frame: background results, 401s, auto-lock, dialogs.
    /// Returns upload-info results for the forms.
    pub fn housekeeping(&mut self, ctx: &egui::Context) -> Vec<(i64, bool, UploadInfo)> {
        let mut infos = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Claimed(r) => self.finish_claim(r),
                Msg::NewVersion(v) => {
                    if !self.update_notified {
                        self.update_notified = true;
                        self.notices.push((
                            format!("Er is een nieuwe versie van de recorder: {v}. Download hem via AudioShare → Recorders."),
                            false,
                        ));
                    }
                }
                Msg::Role(id, role) => {
                    if let Some(p) = self.store.get_mut(&id) {
                        if p.role != role {
                            p.role = role;
                            self.store.save();
                        }
                    }
                }
                Msg::Programs(id, Ok(list)) => {
                    if let Some(p) = self.store.get_mut(&id) {
                        if list.is_empty() {
                            self.notices.push((
                                format!("AudioShare geeft geen programma's terug voor {}. Laat de beheerder een programma aan je toewijzen.", p.display_name),
                                true,
                            ));
                        }
                        if p.programs != list {
                            p.programs = list;
                            self.store.save();
                        }
                    }
                    self.rebuild_view();
                }
                Msg::Programs(id, Err(e)) => {
                    if e.is_temporary() {
                        // Offline: keep using the cached list.
                        self.notices.push((format!("Programmalijst niet opgehaald (bewaarde lijst wordt gebruikt): {}", e.user_message()), true));
                    } else if e.code() == "INVALID_TOKEN" {
                        self.handle_api_error(&id, &e);
                    } else {
                        self.notices.push((format!("Programmalijst ophalen mislukt: {}", e.user_message()), true));
                    }
                }
                Msg::Info(pid, ed, Ok(info)) => infos.push((pid, ed, info)),
                Msg::Info(_, _, Err(e)) => {
                    if let Some(id) = self.active.clone() {
                        if !e.is_temporary() {
                            self.handle_api_error(&id, &e);
                        }
                    }
                }
                Msg::Tested(id, r) => match r {
                    Ok(t) => self.test_msg = Some((t, false)),
                    Err(e) => {
                        self.test_msg = Some((e.user_message(), true));
                        if e.code() == "INVALID_TOKEN" {
                            self.unlinked(&id);
                        }
                    }
                },
            }
        }
        let unl: Vec<String> = std::mem::take(&mut *self.queue.unlinked.lock().unwrap());
        for id in unl {
            self.unlinked(&id);
        }
        if self.store.shared {
            // Activity keeps the profile unlocked; idle time locks it.
            if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
                self.last_activity = Instant::now();
            }
            let limit = Duration::from_secs(self.store.auto_lock_minutes.max(1) as u64 * 60);
            if self.active.is_some() && self.last_activity.elapsed() > limit {
                let n = self.active_name().unwrap_or_default();
                self.sign_out();
                self.notices.push((format!("{n} is automatisch uitgelogd (geen activiteit)"), false));
            }
            // Tokens of signed-out makers leave memory once their uploads are done.
            let ids: Vec<String> = self.queue.tokens.lock().unwrap().keys().cloned().collect();
            for id in ids {
                if Some(&id) != self.active.as_ref() {
                    self.release_token(&id);
                }
            }
        }
        // Ask about replacing as soon as a job of the signed-in maker needs it.
        if self.confirm_job.is_none() {
            if let Some(active) = &self.active {
                let d = self.queue.data.lock().unwrap();
                if let Some(j) = d.jobs.iter().find(|j| {
                    j.state == JobState::NeedsConfirm && &j.profile_id == active && !self.shown_confirm.contains(&j.id)
                }) {
                    self.confirm_job = Some(j.id.clone());
                }
            }
        }
        infos
    }

    fn start_claim(&mut self) {
        let shared = self.store.shared;
        let Some(f) = self.link.as_mut() else { return };
        f.error = None;
        let url = api::normalize_url(&f.url);
        let code = f.code.trim().to_uppercase();
        let device = f.device.trim().to_string();
        if f.url.trim().is_empty() {
            f.error = Some("Vul het adres van AudioShare in (bijv. https://platform.br6.nl), of kies het koppelbestand.".into());
            return;
        }
        if code.is_empty() {
            f.error = Some("Vul de koppelcode in, of kies het koppelbestand.".into());
            return;
        }
        if device.is_empty() {
            f.error = Some("Geef dit apparaat een naam.".into());
            return;
        }
        if shared {
            if f.pin.len() < 4 || !f.pin.chars().all(|c| c.is_ascii_digit()) {
                f.error = Some("De pincode moet uit minimaal 4 cijfers bestaan (liefst 6).".into());
                return;
            }
            if f.pin != f.pin2 {
                f.error = Some("De twee pincodes zijn niet gelijk.".into());
                return;
            }
        }
        f.busy = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = (|| {
                let c = Client::new(&url, None);
                let claim = c.claim(&code, &device).map_err(|e| e.user_message())?;
                let c = Client::new(&url, Some(claim.token.clone()));
                let role = c.me().map(|m| m.user.role).unwrap_or_default();
                let account_type = claim.user.account_type.clone();
                let (programs, warn) = match c.programs() {
                    Ok(p) => (p, None),
                    Err(e) => (Vec::new(), Some(e.user_message())),
                };
                let p = Profile {
                    id: uuid::Uuid::new_v4().to_string(),
                    display_name: claim.user.display_name.clone(),
                    user_id: claim.user.id,
                    device_name: device,
                    url,
                    linked_at: chrono::Local::now().format("%d-%m-%Y %H:%M").to_string(),
                    programs,
                    role,
                    account_type,
                    ..Default::default()
                };
                Ok((p, claim.token, warn))
            })();
            let _ = tx.send(Msg::Claimed(r));
        });
    }

    fn finish_claim(&mut self, r: Result<(Profile, String, Option<String>), String>) {
        let pin = self.link.as_ref().map(|f| f.pin.clone());
        let file = self.link.as_ref().and_then(|f| f.file_path.clone());
        let manager = self.link.as_ref().map(|f| f.manager_name.clone()).unwrap_or_default();
        let res = r.and_then(|(p, token, warn)| {
            if p.is_studio() {
                let studio = p.display_name.clone();
                let names: Vec<String> = p.programs.iter().map(|x| x.name.clone()).collect();
                let (_, new_manager) = self.store.add_studio(p, &token, &manager, pin.as_deref().unwrap_or(""))?;
                if let Some(f) = &file {
                    api::retire_link_file(f);
                }
                if let Some(sid) = self.studio_id() {
                    self.queue.tokens.lock().unwrap().insert(sid.clone(), (self.store.get(&sid).map(|x| x.url.clone()).unwrap_or_default(), token));
                }
                self.sign_out();
                let who = match &new_manager {
                    Some(mid) => {
                        self.studio_sign_in(mid);
                        format!("Je bent ingelogd als studiobeheerder {}.", self.operator())
                    }
                    None => "De bestaande gebruikers van deze pc zijn behouden.".to_string(),
                };
                let progs = if names.is_empty() {
                    "AudioShare geeft nog geen programma's voor deze studio: laat de admin ze aanvinken (Recorders → Studio's).".to_string()
                } else {
                    format!("Programma's: {}.", names.join(", "))
                };
                if let Some(w) = warn {
                    return Ok(format!("Deze pc is gekoppeld als {studio}. {who} Programmalijst ophalen mislukte: {w}"));
                }
                return Ok(format!("Deze pc is gekoppeld als {studio}. {who} {progs}"));
            }
            let names: Vec<String> = p.programs.iter().map(|x| x.name.clone()).collect();
            let who = p.display_name.clone();
            let id = self.store.add(p, &token, pin.as_deref())?;
            if let Some(f) = &file {
                api::retire_link_file(f);
            }
            self.activate(&id, token);
            if let Some(w) = warn {
                return Ok(format!("Gekoppeld als {who}, maar de programmalijst ophalen mislukte: {w}"));
            }
            if names.is_empty() {
                return Ok(format!(
                    "Gekoppeld als {who}, maar AudioShare geeft geen programma's terug. Laat de beheerder een programma aan je toewijzen."
                ));
            }
            Ok(format!(
                "Gekoppeld als {who} — {} programma{}: {}",
                names.len(),
                if names.len() == 1 { "" } else { "'s" },
                names.join(", ")
            ))
        });
        if let Some(f) = self.link.as_mut() {
            f.busy = false;
            f.pin.clear();
            f.pin2.clear();
            match res {
                Ok(m) => f.done = Some(m),
                Err(e) => f.error = Some(e),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Top bar
    // -----------------------------------------------------------------------

    pub fn top_chip(&mut self, ui: &mut egui::Ui) {
        if !self.configured() {
            if ibutton(ui, None, "Koppelen met AudioShare", true, None).clicked() {
                self.open_link(None);
            }
            return;
        }
        if self.store.shared {
            match self.active_name() {
                Some(n) => {
                    if ibutton(ui, None, "Uitloggen", true, None).clicked() {
                        self.sign_out();
                    }
                    let badge = if self.is_admin() { " (beheerder)" } else { "" };
                    let studio = self.store.studio().map(|s| format!(" · {}", s.display_name)).unwrap_or_default();
                    ui.label(egui::RichText::new(format!("Ingelogd als {n}{badge}{studio}")).strong().color(GREEN));
                }
                None => {
                    if ibutton(ui, None, "Inloggen", true, Some(Color32::from_rgb(0x1f, 0x5f, 0x9e))).clicked() {
                        self.unlock = Some(UnlockForm { focus: true, ..Default::default() });
                    }
                    ui.label(egui::RichText::new("Niet ingelogd").color(DIM));
                }
            }
        } else {
            match self.active_name() {
                Some(n) => {
                    ui.label(egui::RichText::new(format!("AudioShare: {n}")).color(GREEN));
                }
                None => {
                    if ibutton(ui, None, "Opnieuw koppelen", true, Some(RED.gamma_multiply(0.7))).clicked() {
                        self.open_link(None);
                    }
                    ui.label(egui::RichText::new("AudioShare ontkoppeld").color(RED));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------------

    pub fn windows(&mut self, ctx: &egui::Context) {
        self.link_window(ctx);
        if self.is_studio() {
            self.studio_unlock_window(ctx);
        } else {
            self.unlock_window(ctx);
        }
        self.confirm_window(ctx);
        self.users_window(ctx);
    }

    fn link_window(&mut self, ctx: &egui::Context) {
        let shared = self.store.shared;
        let Some(f) = self.link.as_mut() else { return };
        let mut open = true;
        let mut do_claim = false;
        let mut pick_file = false;
        let mut close = false;
        egui::Window::new("Koppelen met AudioShare")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if let Some(done) = &f.done {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(done).size(16.0).strong().color(GREEN));
                    ui.add_space(6.0);
                    if f.studio {
                        ui.label("Maak nu onder Instellingen → Gebruikers de technici en makers aan, met hun programma's.");
                    } else {
                        ui.label("Je programma's staan nu in de keuzelijst op het opnamescherm.");
                    }
                    ui.add_space(10.0);
                    if ibutton(ui, None, "Klaar", true, Some(Color32::from_rgb(0x1f, 0x7a, 0x52))).clicked() {
                        close = true;
                    }
                    return;
                }
                if f.studio {
                    ui.label(egui::RichText::new("Deze pc wordt gekoppeld als studio.").strong());
                    ui.label("Wie hier werkt, logt straks in als lokale gebruiker. Kies nu de naam en pincode van de studiobeheerder: die maakt de gebruikers aan en wijst hun programma's toe.");
                } else {
                    ui.label("Je beheerder heeft je een persoonlijke koppelcode of een koppelbestand (.audioshare) gegeven.");
                }
                if shared && !f.studio {
                    ui.label(
                        egui::RichText::new("Elke maker koppelt zijn eigen code en kiest een eigen pincode om mee in te loggen.").color(PURPLE),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ibutton(ui, Some(Icon::Folder), "Koppelbestand kiezen…", !f.busy, None).clicked() {
                        pick_file = true;
                    }
                    if let Some(src) = &f.from_file {
                        ui.label(egui::RichText::new(format!("Ingevuld uit {src}")).color(GREEN));
                    } else {
                        ui.label(egui::RichText::new("of sleep het bestand op dit venster, of vul hieronder in").color(DIM));
                    }
                });
                ui.add_space(8.0);
                egui::Grid::new("link").num_columns(2).spacing(vec2(12.0, 10.0)).show(ui, |ui| {
                    ui.label("Adres");
                    ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.url).hint_text(egui::RichText::new("bijv. https://platform.br6.nl").color(HINT)).desired_width(320.0));
                    ui.end_row();
                    ui.label("Koppelcode");
                    ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.code).hint_text(egui::RichText::new("bijv. K7QX-M2PL-9RTA").color(HINT)).desired_width(200.0));
                    ui.end_row();
                    ui.label("Naam van dit apparaat");
                    ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.device).desired_width(200.0));
                    ui.end_row();
                    if f.studio {
                        ui.label("Naam studiobeheerder");
                        ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.manager_name).hint_text(egui::RichText::new("bijv. Anne").color(HINT)).desired_width(200.0));
                        ui.end_row();
                    }
                    if shared {
                        ui.label(if f.studio { "Beheerpincode" } else { "Kies een pincode" });
                        ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.pin).password(true).hint_text(egui::RichText::new("minimaal 4 cijfers").color(HINT)).desired_width(140.0));
                        ui.end_row();
                        ui.label("Herhaal pincode");
                        ui.add_enabled(!f.busy, egui::TextEdit::singleline(&mut f.pin2).password(true).desired_width(140.0));
                        ui.end_row();
                    }
                });
                if shared {
                    ui.label(
                        egui::RichText::new("Pincode vergeten? Dan maakt de beheerder een nieuwe koppelcode; de recorder kan hem niet herstellen.")
                            .size(12.0)
                            .color(DIM),
                    );
                }
                if let Some(e) = &f.error {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(e).color(RED));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let label = if f.busy { "Bezig met koppelen…" } else { "Koppelen" };
                    if ibutton(ui, None, label, !f.busy, Some(Color32::from_rgb(0x1f, 0x7a, 0x52))).clicked() {
                        do_claim = true;
                    }
                    if f.busy {
                        ui.spinner();
                    }
                });
            });
        if pick_file {
            if let Some(p) = rfd::FileDialog::new().add_filter("AudioShare koppelbestand", &["audioshare"]).pick_file() {
                let (pin, pin2, device, mgr) = (f.pin.clone(), f.pin2.clone(), f.device.clone(), f.manager_name.clone());
                self.open_link(Some(&p));
                if let Some(nf) = self.link.as_mut() {
                    nf.pin = pin;
                    nf.pin2 = pin2;
                    nf.device = device;
                    nf.manager_name = mgr;
                }
            }
            return;
        }
        if do_claim {
            self.start_claim();
        }
        if !open || close {
            self.link = None;
        }
    }

    fn unlock_window(&mut self, ctx: &egui::Context) {
        if self.unlock.is_none() {
            return;
        }
        let mut open = true;
        let mut try_pin: Option<(String, String)> = None;
        let mut relink = false;
        let mut set_pin: Option<(String, String)> = None;
        let profiles: Vec<Profile> = self.store.profiles.clone();
        let needs_pin: HashSet<String> = profiles.iter().filter(|p| self.store.needs_pin(&p.id)).map(|p| p.id.clone()).collect();
        let f = self.unlock.as_mut().unwrap();
        if f.selected.is_none() && profiles.len() == 1 {
            f.selected = Some(profiles[0].id.clone());
            f.focus = true;
        }
        egui::Window::new("Inloggen")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("Wie ben je?");
                ui.add_space(6.0);
                for p in &profiles {
                    let sel = f.selected.as_deref() == Some(&p.id);
                    let mut text = egui::RichText::new(&p.display_name).size(15.0);
                    if p.needs_relink {
                        text = text.color(DIM);
                    }
                    if ui.add_sized([380.0, 32.0], egui::Button::selectable(sel, text)).clicked() {
                        f.selected = Some(p.id.clone());
                        f.pin.clear();
                        f.pin2.clear();
                        f.msg = None;
                        f.focus = true;
                    }
                }
                ui.add_space(6.0);
                if let Some(id) = f.selected.clone() {
                    let p = profiles.iter().find(|p| p.id == id);
                    if p.is_some_and(|p| p.needs_relink) {
                        ui.label(egui::RichText::new("Dit profiel moet opnieuw gekoppeld worden met een nieuwe code van de beheerder.").color(AMBER));
                        if ui.button("Opnieuw koppelen").clicked() {
                            relink = true;
                        }
                    } else if needs_pin.contains(&id) {
                        ui.label(egui::RichText::new("Kies een pincode om voortaan mee in te loggen (minimaal 4 cijfers).").color(PURPLE));
                        ui.add_space(4.0);
                        egui::Grid::new("newpin").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                            ui.label("Pincode");
                            let r = ui.add(egui::TextEdit::singleline(&mut f.pin).password(true).desired_width(140.0));
                            if f.focus {
                                r.request_focus();
                                f.focus = false;
                            }
                            ui.end_row();
                            ui.label("Herhaal");
                            let r2 = ui.add(egui::TextEdit::singleline(&mut f.pin2).password(true).desired_width(140.0));
                            ui.end_row();
                            let enter = r2.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            ui.label("");
                            if ui.button("Opslaan en inloggen").clicked() || enter {
                                if f.pin.len() < 4 || !f.pin.chars().all(|c| c.is_ascii_digit()) {
                                    f.msg = Some("De pincode moet uit minimaal 4 cijfers bestaan.".into());
                                } else if f.pin != f.pin2 {
                                    f.msg = Some("De twee pincodes zijn niet gelijk.".into());
                                } else {
                                    set_pin = Some((id.clone(), f.pin.clone()));
                                }
                            }
                            ui.end_row();
                        });
                    } else {
                        ui.horizontal(|ui| {
                            ui.label("Pincode");
                            let r = ui.add(egui::TextEdit::singleline(&mut f.pin).password(true).desired_width(140.0));
                            if f.focus {
                                r.request_focus();
                                f.focus = false;
                            }
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if ui.button("Inloggen").clicked() || enter {
                                try_pin = Some((id.clone(), f.pin.clone()));
                            }
                        });
                    }
                }
                if let Some(m) = &f.msg {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(m).color(RED));
                }
                ui.add_space(8.0);
                ui.separator();
                if ui.link("Nieuwe maker koppelen…").clicked() {
                    relink = true;
                }
            });
        if relink {
            self.open_link(None);
            return;
        }
        if let Some((id, pin)) = set_pin {
            match self.store.set_pin(&id, &pin) {
                Ok(token) => {
                    self.unlock = None;
                    self.activate(&id, token);
                    return;
                }
                Err(e) => {
                    let f = self.unlock.as_mut().unwrap();
                    f.msg = Some(e);
                }
            }
        }
        if let Some((id, pin)) = try_pin {
            let r = self.store.unlock(&id, &pin);
            let f = self.unlock.as_mut().unwrap();
            f.pin.clear();
            f.focus = true;
            match r {
                Unlock::Ok(token) => {
                    self.unlock = None;
                    self.activate(&id, token);
                    return;
                }
                Unlock::Wrong { attempts_left } => {
                    f.msg = Some(format!("Onjuiste pincode — nog {attempts_left} poging(en) voordat het profiel 5 minuten op slot gaat."))
                }
                Unlock::Locked { seconds } => {
                    f.msg = Some(format!("Te veel foute pogingen. Probeer het over {} opnieuw.", mmss(seconds as f64)))
                }
                Unlock::Relink => {
                    f.msg = Some("Te vaak een onjuiste pincode. Vraag je beheerder om een nieuwe koppelcode en koppel opnieuw.".into())
                }
            }
        }
        if !open {
            self.unlock = None;
        }
    }

    /// Warning before replacing an existing episode (spec §5).
    fn confirm_window(&mut self, ctx: &egui::Context) {
        let Some(jid) = self.confirm_job.clone() else { return };
        let job = self.queue.data.lock().unwrap().jobs.iter().find(|j| j.id == jid).cloned();
        let Some(job) = job.filter(|j| j.state == JobState::NeedsConfirm) else {
            self.confirm_job = None;
            return;
        };
        let d = &job.confirm_details;
        let title = d["current_title"].as_str().unwrap_or(&job.title).to_string();
        let uploaded = d["uploaded_at"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        let downloads: Vec<String> = d["downloaded_by"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        let at = x["at"]
                            .as_str()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|t| t.format(" (%d-%m %H:%M)").to_string())
                            .unwrap_or_default();
                        format!("{}{at}", x["station"].as_str().unwrap_or("?"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut choice: Option<bool> = None;
        egui::Modal::new(egui::Id::new("replace_confirm")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format!("{title} bestaat al")).size(17.0).strong());
                if let Some(t) = uploaded {
                    ui.label(egui::RichText::new(format!("(geüpload {} om {})", t.format("%d-%m"), t.format("%H:%M"))).color(DIM));
                }
            });
            if !downloads.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Let op: al gedownload door {}. Dit station zendt waarschijnlijk de oude versie uit.",
                        downloads.join(", ")
                    ))
                    .color(AMBER),
                );
            }
            ui.add_space(8.0);
            ui.label("Weet je zeker dat je wilt vervangen?");
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ibutton(ui, None, "Annuleren", true, None).clicked() {
                    choice = Some(false);
                }
                if ibutton(ui, None, "Vervangen", true, Some(RED)).clicked() {
                    choice = Some(true);
                }
            });
        });
        match choice {
            Some(true) => {
                self.queue.confirm_replace(&jid);
                self.shown_confirm.insert(jid);
                self.confirm_job = None;
            }
            Some(false) => {
                self.queue.cancel(&jid);
                self.shown_confirm.insert(jid);
                self.confirm_job = None;
            }
            None => {}
        }
    }

    // -----------------------------------------------------------------------
    // Uploads screen
    // -----------------------------------------------------------------------

    pub fn uploads_screen(&mut self, ui: &mut egui::Ui) {
        if !self.configured() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("Nog niet gekoppeld met AudioShare").size(20.0));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Koppel de recorder met de code of het koppelbestand van je beheerder.").color(DIM));
                ui.add_space(10.0);
                if ibutton(ui, None, "Koppelen met AudioShare", true, Some(Color32::from_rgb(0x1f, 0x7a, 0x52))).clicked() {
                    self.open_link(None);
                }
            });
            return;
        }
        let Some(profile) = self.active_profile().cloned() else {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("Log in om je uploads te zien").size(20.0));
                ui.add_space(10.0);
                if ibutton(ui, None, "Inloggen", true, Some(Color32::from_rgb(0x1f, 0x5f, 0x9e))).clicked() {
                    self.unlock = Some(UnlockForm { focus: true, ..Default::default() });
                }
            });
            return;
        };
        let (jobs, log) = {
            let d = self.queue.data.lock().unwrap();
            // An admin sees everyone's uploads on this PC.
            let all = profile.is_admin();
            let studio = profile.is_studio();
            let jobs: Vec<Job> = d
                .jobs
                .iter()
                .filter(|j| all || (j.profile_id == profile.id && (!studio || j.operator == profile.display_name)))
                .cloned()
                .collect();
            let log: Vec<_> = d.log.iter().filter(|l| all || l.profile == profile.display_name).cloned().collect();
            (jobs, log)
        };
        let progress = self.queue.progress.lock().unwrap().as_ref().map(|(id, sent, total)| (id.clone(), sent.load(Ordering::Relaxed), *total));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Uploadwachtrij").size(18.0).strong());
            let who = if profile.is_admin() {
                if profile.is_studio() { "iedereen in deze studio (beheerder)".to_string() } else { "alle makers (beheerder)".to_string() }
            } else {
                profile.display_name.clone()
            };
            ui.label(egui::RichText::new(format!("— {who}")).color(DIM));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ibutton(ui, None, "Afgeronde verwijderen", jobs.iter().any(|j| !j.state.is_open()), None).clicked() {
                    self.queue.remove_finished();
                }
            });
        });
        ui.add_space(6.0);
        let avail = ui.available_height();
        egui::ScrollArea::vertical().id_salt("jobs").max_height((avail * 0.6).max(160.0)).show(ui, |ui| {
            if jobs.is_empty() {
                ui.label(egui::RichText::new("Geen uploads. Een export met 'uploaden naar AudioShare' aangevinkt komt hier te staan.").color(DIM));
            }
            for j in jobs.iter().rev() {
                self.job_card(ui, j, progress.as_ref());
                ui.add_space(6.0);
            }
        });
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Logboek").strong());
        egui::Frame::NONE.fill(PANEL).corner_radius(6.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().id_salt("log").stick_to_bottom(true).show(ui, |ui| {
                for l in &log {
                    let col = match l.level {
                        Level::Ok => GREEN,
                        Level::Warn => AMBER,
                        Level::Error => RED,
                        Level::Info => DIM,
                    };
                    ui.label(egui::RichText::new(format!("{}  [{}] {}", l.time, l.profile, l.text)).color(col).monospace().size(12.0));
                }
            });
        });
    }

    fn job_card(&mut self, ui: &mut egui::Ui, j: &Job, progress: Option<&(String, u64, u64)>) {
        let (label, col) = match j.state {
            JobState::Waiting => ("Wacht", DIM),
            JobState::Creating => ("Aanmaken", WAVE),
            JobState::Uploading => ("Uploaden", WAVE),
            JobState::Finalizing => ("Vrijgeven", WAVE),
            JobState::NeedsConfirm => ("Bevestigen", AMBER),
            JobState::Done => ("Klaar", GREEN),
            JobState::Failed => ("Mislukt", RED),
            JobState::Cancelled => ("Geannuleerd", DIM),
        };
        egui::Frame::NONE.fill(PANEL).corner_radius(8.0).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(92.0, 22.0), Sense::hover());
                ui.painter().rect_filled(r, 4.0, col.gamma_multiply(0.25));
                ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.5), col);
                ui.label(egui::RichText::new(format!("{} — {}", j.program_name, j.title)).strong());
                let date = chrono::NaiveDate::parse_from_str(&j.date, "%Y-%m-%d").map(|d| d.format("%d-%m-%Y").to_string()).unwrap_or(j.date.clone());
                ui.label(egui::RichText::new(format!("uitzenddag {date}")).color(DIM));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if j.state == JobState::NeedsConfirm && ui.button("Vervangen…").clicked() {
                        self.shown_confirm.remove(&j.id);
                        self.confirm_job = Some(j.id.clone());
                    }
                    if j.state == JobState::Failed && ui.button("Opnieuw").clicked() {
                        self.queue.retry(&j.id);
                    }
                    if j.state.is_open() && ui.button("Annuleren").clicked() {
                        self.queue.cancel(&j.id);
                    }
                    if ui.small_button("Toon bestand").clicked() {
                        show_in_folder(&j.file);
                    }
                });
            });
            if let Some((_, sent, total)) = progress.filter(|p| p.0 == j.id) {
                let f = if *total > 0 { *sent as f32 / *total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(f).text(format!("{:.0}%  ({} / {} MB)", f * 100.0, sent / 1_048_576, total / 1_048_576)));
            } else if !j.message.is_empty() {
                let mcol = if j.state == JobState::Failed { RED } else { DIM };
                ui.label(egui::RichText::new(&j.message).color(mcol));
            }
            if j.state == JobState::Waiting && !self.queue.tokens.lock().unwrap().contains_key(&j.profile_id) {
                ui.label(egui::RichText::new(format!("Wacht tot {} inlogt", j.profile_name)).color(AMBER));
            }
            if !j.operator.is_empty() {
                ui.label(egui::RichText::new(format!("Bediend door {}", j.operator)).color(DIM).size(12.0));
            }
            for w in &j.warnings {
                ui.label(egui::RichText::new(&w.message).color(AMBER));
            }
        });
    }

    // -----------------------------------------------------------------------
    // Settings section
    // -----------------------------------------------------------------------

    pub fn settings_section(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("AudioShare").strong().size(15.0));
        ui.add_space(4.0);
        if self.store.shared {
            ui.horizontal(|ui| {
                ui.label("Automatisch uitloggen na");
                let mut m = self.store.auto_lock_minutes;
                if ui.add(egui::DragValue::new(&mut m).range(1..=240).suffix(" min")).changed() {
                    self.store.auto_lock_minutes = m;
                    self.store.save();
                }
                ui.label("zonder activiteit");
            });
        }
        ui.add_space(4.0);
        let mut forget: Option<String> = None;
        let mut test: Option<String> = None;
        for p in &self.store.profiles {
            ui.horizontal(|ui| {
                let active = self.active.as_deref() == Some(&p.id);
                let dot = if p.needs_relink { RED } else if active { GREEN } else { DIM };
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 16.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.5, dot);
                ui.label(egui::RichText::new(&p.display_name).strong());
                ui.label(egui::RichText::new(format!("{} · {} · gekoppeld {}", p.device_name, p.url, p.linked_at)).color(DIM).size(12.0));
                if active && ui.small_button("Verbinding testen").clicked() {
                    test = Some(p.id.clone());
                }
                if ui.small_button("Vergeten").on_hover_text("Verwijdert de koppeling van deze pc. Ontkoppel het apparaat ook in AudioShare (profielpagina).").clicked() {
                    forget = Some(p.id.clone());
                }
            });
        }
        if let Some((m, err)) = &self.test_msg {
            ui.label(egui::RichText::new(m).color(if *err { RED } else { GREEN }));
        }
        if self.is_studio() {
            self.studio_settings(ui);
        } else if ui.button("Maker koppelen…").clicked() {
            self.open_link(None);
        }
        if let Some(id) = test {
            if let Some(c) = self.client() {
                let tx = self.tx.clone();
                self.test_msg = Some(("Bezig…".into(), false));
                std::thread::spawn(move || {
                    let r = c.me().map(|me| {
                        let progs = c.programs();
                        let report = c.diagnose();
                        // Next to the program and in the settings folder.
                        let mut saved = Vec::new();
                        let mut places = vec![crate::settings::config_dir()];
                        if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
                            places.insert(0, d);
                        }
                        for d in places {
                            let f = d.join("audioshare-diagnose.txt");
                            if std::fs::create_dir_all(&d).is_ok() && std::fs::write(&f, &report).is_ok() {
                                saved.push(f.display().to_string());
                            }
                        }
                        let progs = match progs {
                            Ok(p) if p.is_empty() => "AudioShare geeft geen programma's terug".to_string(),
                            Ok(p) => format!("{} programma('s): {}", p.len(), p.iter().map(|x| x.name.clone()).collect::<Vec<_>>().join(", ")),
                            Err(e) => format!("programmalijst mislukt: {}", e.user_message()),
                        };
                        format!(
                            "Verbinding in orde als {} — {progs}.\nDetails: {}",
                            me.user.display_name,
                            saved.first().cloned().unwrap_or_else(|| "(niet opgeslagen)".into())
                        )
                    });
                    let _ = tx.send(Msg::Tested(id, r));
                });
                self.refresh_programs();
            }
        }
        if let Some(id) = forget {
            if self.active.as_deref() == Some(&id) {
                self.active = None;
            }
            self.queue.tokens.lock().unwrap().remove(&id);
            self.store.remove(&id);
        }
        let _ = profiles::now;
    }

    /// Open jobs of the signed-in maker (for the tab badge).
    pub fn open_count(&self) -> usize {
        let Some(id) = &self.active else { return 0 };
        self.queue.data.lock().unwrap().jobs.iter().filter(|j| &j.profile_id == id && j.state.is_open()).count()
    }
}

// ---------------------------------------------------------------------------
// Studio mode (AudioShare 1.5.0): local users with a PIN, chosen by the
// studio manager, each with a selection of the studio's programmes.
// ---------------------------------------------------------------------------

impl AsUi {
    fn studio_unlock_window(&mut self, ctx: &egui::Context) {
        if self.unlock.is_none() {
            return;
        }
        let Some(studio) = self.store.studio().cloned() else { return };
        let users = self.store.local_users.clone();
        let mut open = true;
        let mut try_pin: Option<(String, String)> = None;
        let mut relink = false;
        let f = self.unlock.as_mut().unwrap();
        if f.selected.is_none() && users.len() == 1 {
            f.selected = Some(users[0].id.clone());
            f.focus = true;
        }
        egui::Window::new(format!("Inloggen — {}", studio.display_name))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if studio.needs_relink {
                    ui.label(egui::RichText::new("Deze studio is in AudioShare ontkoppeld. Opnemen en bewerken kan; uploaden pas na opnieuw koppelen met een nieuwe code van de admin.").color(AMBER));
                    if ui.button("Studio opnieuw koppelen…").clicked() {
                        relink = true;
                    }
                    ui.add_space(6.0);
                }
                ui.label("Wie ben je?");
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for u in &users {
                        let sel = f.selected.as_deref() == Some(&u.id);
                        let label = if u.manager { format!("{}  (beheerder)", u.name) } else { u.name.clone() };
                        if ui.add_sized([380.0, 32.0], egui::Button::selectable(sel, egui::RichText::new(label).size(15.0))).clicked() {
                            f.selected = Some(u.id.clone());
                            f.pin.clear();
                            f.msg = None;
                            f.focus = true;
                        }
                    }
                });
                ui.add_space(6.0);
                if let Some(id) = f.selected.clone() {
                    ui.horizontal(|ui| {
                        ui.label("Pincode");
                        let r = ui.add(egui::TextEdit::singleline(&mut f.pin).password(true).desired_width(140.0));
                        if f.focus {
                            r.request_focus();
                            f.focus = false;
                        }
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if ui.button("Inloggen").clicked() || enter {
                            try_pin = Some((id, f.pin.clone()));
                        }
                    });
                }
                if let Some(m) = &f.msg {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(m).color(RED));
                }
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Nog geen gebruiker of pincode vergeten? Vraag de studiobeheerder.").size(12.0).color(DIM));
            });
        if relink {
            self.unlock = None;
            self.open_link(None);
            return;
        }
        if let Some((id, pin)) = try_pin {
            let r = self.store.unlock_local(&id, &pin);
            let f = self.unlock.as_mut().unwrap();
            f.pin.clear();
            f.focus = true;
            match r {
                LocalUnlock::Ok => {
                    self.unlock = None;
                    self.studio_sign_in(&id);
                    return;
                }
                LocalUnlock::Wrong { attempts_left } => {
                    f.msg = Some(format!("Onjuiste pincode — nog {attempts_left} poging(en) voordat je 5 minuten moet wachten."))
                }
                LocalUnlock::Locked { seconds } => {
                    f.msg = Some(format!("Te veel foute pogingen. Probeer het over {} opnieuw, of vraag de studiobeheerder om een nieuwe pincode.", mmss(seconds as f64)))
                }
            }
        }
        if !open {
            self.unlock = None;
        }
    }

    /// Settings → AudioShare, studio part (only reachable for a manager).
    fn studio_settings(&mut self, ui: &mut egui::Ui) {
        let Some(st) = self.store.studio().cloned() else { return };
        ui.add_space(6.0);
        ui.label(egui::RichText::new(format!("Studio: {}", st.display_name)).strong());
        let progs: Vec<String> = st.programs.iter().map(|p| p.name.clone()).collect();
        ui.label(
            egui::RichText::new(if progs.is_empty() {
                "AudioShare staat voor deze studio nog geen programma's toe (de admin vinkt ze aan bij Recorders → Studio's).".to_string()
            } else {
                format!("Mag aanleveren voor: {}", progs.join(", "))
            })
            .color(DIM)
            .size(12.5),
        );
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Gebruikers").strong());
        let users = self.store.local_users.clone();
        let mut edit: Option<String> = None;
        egui::Grid::new("local_users").num_columns(3).spacing(vec2(14.0, 6.0)).show(ui, |ui| {
            for u in &users {
                let name = if u.manager { format!("{} (beheerder)", u.name) } else { u.name.clone() };
                ui.label(egui::RichText::new(name).strong());
                let mine: Vec<String> = if u.manager {
                    vec!["alle programma's".into()]
                } else {
                    st.programs.iter().filter(|p| u.programs.contains(&p.id)).map(|p| p.name.clone()).collect()
                };
                ui.label(
                    egui::RichText::new(if mine.is_empty() { "nog geen programma's".to_string() } else { mine.join(", ") })
                        .color(if mine.is_empty() { AMBER } else { DIM })
                        .size(12.5),
                );
                if ui.small_button("Wijzigen…").clicked() {
                    edit = Some(u.id.clone());
                }
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Gebruiker toevoegen…").clicked() {
                self.users_form = Some(UsersForm::default());
            }
            if ui.button("Studio opnieuw koppelen…").on_hover_text("Met een nieuwe koppelcode van de admin. Gebruikers en instellingen blijven behouden.").clicked() {
                self.open_link(None);
            }
        });
        if let Some(id) = edit {
            if let Some(u) = self.store.local_user(&id) {
                self.users_form = Some(UsersForm {
                    editing: Some(u.id.clone()),
                    name: u.name.clone(),
                    manager: u.manager,
                    programs: u.programs.iter().copied().collect(),
                    ..Default::default()
                });
            }
        }
    }

    fn users_window(&mut self, ctx: &egui::Context) {
        if self.users_form.is_none() || !self.is_admin() {
            return;
        }
        let Some(st) = self.store.studio().cloned() else { return };
        let mut open = true;
        let mut save = false;
        let mut delete = false;
        let f = self.users_form.as_mut().unwrap();
        let title = if f.editing.is_some() { "Gebruiker wijzigen" } else { "Gebruiker toevoegen" };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Grid::new("user_form").num_columns(2).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
                    ui.label("Naam");
                    ui.add(egui::TextEdit::singleline(&mut f.name).hint_text(egui::RichText::new("bijv. Kees").color(HINT)).desired_width(220.0));
                    ui.end_row();
                    ui.label(if f.editing.is_some() { "Nieuwe pincode" } else { "Pincode" });
                    ui.add(egui::TextEdit::singleline(&mut f.pin).password(true).hint_text(egui::RichText::new(if f.editing.is_some() { "leeg = ongewijzigd" } else { "minimaal 4 cijfers" }).color(HINT)).desired_width(140.0));
                    ui.end_row();
                    ui.label("Herhaal pincode");
                    ui.add(egui::TextEdit::singleline(&mut f.pin2).password(true).desired_width(140.0));
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut f.manager, "Studiobeheerder (instellingen, gebruikers, alle programma's)");
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Mag uploaden voor").strong());
                if f.manager {
                    ui.label(egui::RichText::new("Een studiobeheerder mag alle programma's van deze studio uploaden.").color(DIM));
                } else if st.programs.is_empty() {
                    ui.label(egui::RichText::new("AudioShare staat voor deze studio nog geen programma's toe.").color(AMBER));
                } else {
                    for p in &st.programs {
                        let mut on = f.programs.contains(&p.id);
                        if ui.checkbox(&mut on, &p.name).changed() {
                            if on {
                                f.programs.insert(p.id);
                            } else {
                                f.programs.remove(&p.id);
                            }
                        }
                    }
                }
                if let Some((m, err)) = &f.msg {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(m).color(if *err { RED } else { GREEN }));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ibutton(ui, None, "Opslaan", true, Some(Color32::from_rgb(0x1f, 0x7a, 0x52))).clicked() {
                        save = true;
                    }
                    if f.editing.is_some() && ui.button("Verwijderen").clicked() {
                        delete = true;
                    }
                });
            });
        if save {
            let f = self.users_form.as_mut().unwrap();
            let programs: Vec<i64> = st.programs.iter().map(|p| p.id).filter(|id| f.programs.contains(id)).collect();
            let r = if !f.pin.is_empty() && f.pin != f.pin2 {
                Err("De twee pincodes zijn niet gelijk.".to_string())
            } else {
                match f.editing.clone() {
                    None => self.store.add_local_user(&f.name, &f.pin, programs, f.manager).map(|_| ()),
                    Some(id) => self.store.update_local_user(&id, &f.name, programs, f.manager).and_then(|_| {
                        if f.pin.is_empty() { Ok(()) } else { self.store.set_local_pin(&id, &f.pin) }
                    }),
                }
            };
            match r {
                Ok(()) => {
                    self.users_form = None;
                    self.rebuild_view();
                    // A manager who took away their own role is no longer a manager
                    return;
                }
                Err(e) => self.users_form.as_mut().unwrap().msg = Some((e, true)),
            }
        }
        if delete {
            let id = self.users_form.as_ref().and_then(|f| f.editing.clone()).unwrap_or_default();
            if self.local_user.as_deref() == Some(id.as_str()) {
                self.users_form.as_mut().unwrap().msg = Some(("Je kunt jezelf niet verwijderen terwijl je bent ingelogd.".into(), true));
            } else {
                match self.store.remove_local_user(&id) {
                    Ok(()) => self.users_form = None,
                    Err(e) => self.users_form.as_mut().unwrap().msg = Some((e, true)),
                }
            }
        }
        if !open {
            self.users_form = None;
        }
    }
}
