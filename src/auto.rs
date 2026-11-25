//! Automatische opname (StudioRecorder 0.9.0, spec "Automatische opname via UDP" v0.2).
//!
//! Een eigen script stuurt `START` en `STOP` via UDP. De controller draait in
//! een eigen thread, los van het venster (werkt dus ook geminimaliseerd): hij
//! start en stopt de opname via een `engine::Handle`, en na STOP exporteert hij
//! en slaat hij op in een map en/of zet hij het bestand in de uploadwachtrij.

use crate::audioshare::api::{Client, Program};
use crate::audioshare::queue::{Job, Level, Queue};
use crate::engine::{self, Handle};
use crate::export::{self, ExportFormat, ExportJob};
use crate::meta::{Meta, safe_file_part};
use crate::wav::WavReader;
use chrono::{Datelike, Local, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 47820;
/// Log label in het logboek (tabblad Uploads).
const LOG: &str = "Automatisch";

// ---------------------------------------------------------------------------
// Instellingen
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AfterStop {
    Save,
    Upload,
    #[default]
    Both,
}

impl AfterStop {
    pub fn label(self) -> &'static str {
        match self {
            AfterStop::Save => "Alleen opslaan",
            AfterStop::Upload => "Alleen uploaden",
            AfterStop::Both => "Opslaan + uploaden",
        }
    }
    pub fn saves(self) -> bool {
        matches!(self, AfterStop::Save | AfterStop::Both)
    }
    pub fn uploads(self) -> bool {
        matches!(self, AfterStop::Upload | AfterStop::Both)
    }
}

/// Een programma: uit AudioShare (`id`) of een lokale naam.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProgRef {
    pub id: Option<i64>,
    pub name: String,
}

impl ProgRef {
    pub fn is_set(&self) -> bool {
        self.id.is_some() || !self.name.trim().is_empty()
    }
    /// Sleutel voor de instellingen per programma.
    pub fn key(&self) -> String {
        match self.id {
            Some(id) => format!("as:{id}"),
            None => self.name.trim().to_string(),
        }
    }
}

/// Instellingen per programma (tabblad Automatisch).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgAuto {
    pub after_stop: AfterStop,
    /// Map voor "opslaan" (leeg = <opnamemap>\Automatisch\<programma>).
    pub folder: String,
    pub save_format: ExportFormat,
    pub title_pattern: String,
    /// Omschrijving van de vorige aflevering (AudioShare) gebruiken; anders `description`.
    pub previous_description: bool,
    pub description: String,
    /// Bestandsnaam bij opslaan (leeg = de titel). Zelfde variabelen als de titel.
    pub file_pattern: String,
    /// Een bestaand bestand met dezelfde naam vervangen (anders " (2)" erachter).
    pub overwrite: bool,
}

impl Default for ProgAuto {
    fn default() -> Self {
        ProgAuto {
            // Veilig standaard: uploaden kies je bewust (en kan alleen met een studiokoppeling).
            after_stop: AfterStop::Save,
            folder: String::new(),
            save_format: ExportFormat::Flac,
            title_pattern: "{programma} — {datum}".into(),
            previous_description: true,
            description: String::new(),
            file_pattern: String::new(),
            overwrite: false,
        }
    }
}

/// Een blok in het weekrooster. Dagen: 1 = maandag … 7 = zondag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Slot {
    pub days: Vec<u8>,
    pub from: String,
    pub to: String,
    pub program: ProgRef,
}

impl Default for Slot {
    fn default() -> Self {
        Slot { days: vec![1, 2, 3, 4, 5], from: "09:00".into(), to: "10:00".into(), program: ProgRef::default() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoSettings {
    pub enabled: bool,
    pub port: u16,
    /// Toegestane afzenders (IP-adressen). "*" = iedereen.
    pub allowed: Vec<String>,
    /// Optionele sleutel: elk commando moet `key=<sleutel>` bevatten.
    pub key: String,
    /// Maximale opnameduur zonder STOP, in minuten.
    pub max_minutes: u32,
    pub use_message: bool,
    pub use_schedule: bool,
    pub use_fixed: bool,
    pub fixed: ProgRef,
    pub schedule: Vec<Slot>,
    pub programs: HashMap<String, ProgAuto>,
}

impl Default for AutoSettings {
    fn default() -> Self {
        AutoSettings {
            enabled: false,
            port: DEFAULT_PORT,
            allowed: vec!["127.0.0.1".into()],
            key: String::new(),
            max_minutes: 60,
            use_message: true,
            use_schedule: true,
            use_fixed: true,
            fixed: ProgRef::default(),
            schedule: Vec::new(),
            programs: HashMap::new(),
        }
    }
}

impl AutoSettings {
    pub fn prog(&self, p: &ProgRef) -> ProgAuto {
        self.programs.get(&p.key()).cloned().unwrap_or_default()
    }

    pub fn sender_allowed(&self, ip: IpAddr) -> bool {
        self.allowed.iter().any(|a| {
            let a = a.trim();
            a == "*" || a.parse::<IpAddr>().is_ok_and(|x| x == ip) || (a.eq_ignore_ascii_case("localhost") && ip.is_loopback())
        })
    }

    /// Alleen lokaal luisteren als alleen deze pc commando's mag sturen.
    fn bind_addr(&self) -> [u8; 4] {
        let only_local = !self.allowed.is_empty()
            && self.allowed.iter().all(|a| {
                let a = a.trim();
                a.eq_ignore_ascii_case("localhost") || a.parse::<IpAddr>().is_ok_and(|x| x.is_loopback())
            });
        if only_local { [127, 0, 0, 1] } else { [0, 0, 0, 0] }
    }
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Start { program: Option<String> },
    Stop,
    Status,
}

/// Een datagram ontleden: "START", "START DeBlik1", "STOP", "STATUS",
/// eventueel met `key=…` ergens in de regel.
pub fn parse(text: &str) -> Result<(Command, Option<String>), String> {
    let mut key = None;
    let mut words = Vec::new();
    for w in text.split_whitespace() {
        match w.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("key") => key = Some(v.to_string()),
            _ => words.push(w),
        }
    }
    let Some(first) = words.first() else { return Err("leeg bericht".into()) };
    let rest = words[1..].join(" ");
    let cmd = match first.to_ascii_uppercase().as_str() {
        "START" => Command::Start { program: (!rest.is_empty()).then_some(rest) },
        "STOP" => Command::Stop,
        "STATUS" => Command::Status,
        other => return Err(format!("onbekend commando {other}")),
    };
    Ok((cmd, key))
}

fn hhmm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// Het roosterblok voor dit moment (5 minuten speling vooraf).
/// Een blok dat over middernacht loopt (bijv. 23:00–01:00) telt voor de dag waarop het begint.
pub fn slot_at(schedule: &[Slot], now: chrono::NaiveDateTime) -> Option<&Slot> {
    let lead = chrono::Duration::minutes(5);
    schedule.iter().find(|s| {
        let (Some(from), Some(to)) = (hhmm(&s.from), hhmm(&s.to)) else { return false };
        let from = from - lead;
        [0i64, 1].iter().any(|back| {
            let day = now.date() - chrono::Duration::days(*back);
            let dow = day.weekday().number_from_monday() as u8;
            if !s.days.contains(&dow) {
                return false;
            }
            let start = day.and_time(from);
            let mut end = day.and_time(to);
            if end <= day.and_time(from + lead) {
                end += chrono::Duration::days(1); // over middernacht
            }
            now >= start && now < end
        })
    })
}

/// Titel volgens het patroon: {programma}, {datum} (20 november), {tijd} (21:00), {week}.
pub fn title_from(pattern: &str, program: &str, when: chrono::DateTime<Local>) -> String {
    const MONTHS: [&str; 12] =
        ["januari", "februari", "maart", "april", "mei", "juni", "juli", "augustus", "september", "oktober", "november", "december"];
    let date = format!("{} {}", when.day(), MONTHS[when.month0() as usize]);
    let pattern = if pattern.trim().is_empty() { "{programma} — {datum}" } else { pattern };
    pattern
        .replace("{programma}", program)
        .replace("{datum}", &date)
        .replace("{tijd}", &format!("{:02}:{:02}", when.hour(), when.minute()))
        .replace("{week}", &when.iso_week().week().to_string())
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// De koppeling van deze pc als studio (voor uploaden zonder ingelogde gebruiker).
#[derive(Clone, Debug, Default)]
pub struct StudioLink {
    pub profile_id: String,
    pub profile_name: String,
    pub url: String,
    pub programs: Vec<Program>,
}

/// Wat de controller van het venster nodig heeft; het venster werkt dit bij.
#[derive(Clone, Default)]
pub struct Ctx {
    pub engine: Option<Handle>,
    pub auto: AutoSettings,
    pub record_folder: String,
    pub studio_name: String,
    pub fade_ms: u32,
    /// Lokale programmanamen (zonder AudioShare-koppeling).
    pub local_programs: Vec<String>,
    pub studio: Option<StudioLink>,
}

#[derive(Clone, Debug)]
pub struct Running {
    pub program: ProgRef,
    pub program_name: String,
    pub path: PathBuf,
    pub started: chrono::DateTime<Local>,
    pub since: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// Luistert op deze poort (None = uit of poort bezet).
    pub listening: Option<u16>,
    pub bind_error: Option<String>,
    pub running: Option<Running>,
    /// Exports/uploads die nog worden verwerkt.
    pub processing: u32,
    pub last: Option<(String, bool)>,
}

pub struct Auto {
    pub ctx: Arc<Mutex<Ctx>>,
    pub status: Arc<Mutex<Status>>,
    stop_req: Arc<AtomicBool>,
}

impl Auto {
    pub fn start(queue: Arc<Queue>, repaint: impl Fn() + Send + 'static) -> Auto {
        let a = Auto {
            ctx: Arc::new(Mutex::new(Ctx::default())),
            status: Arc::new(Mutex::new(Status::default())),
            stop_req: Arc::new(AtomicBool::new(false)),
        };
        let (ctx, status, stop_req) = (a.ctx.clone(), a.status.clone(), a.stop_req.clone());
        std::thread::Builder::new()
            .name("auto-record".into())
            .spawn(move || controller(ctx, status, stop_req, queue, Box::new(repaint)))
            .expect("thread");
        a
    }

    pub fn running(&self) -> Option<Running> {
        self.status.lock().unwrap().running.clone()
    }

    /// Handmatig stoppen van een automatische opname (knop in het venster).
    pub fn request_stop(&self) {
        self.stop_req.store(true, Ordering::Relaxed);
    }
}

fn log(queue: &Queue, level: Level, text: impl Into<String>) {
    queue.with(|d| d.add_log(LOG, level, text));
}

fn mmss(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

fn controller(ctx: Arc<Mutex<Ctx>>, status: Arc<Mutex<Status>>, stop_req: Arc<AtomicBool>, queue: Arc<Queue>, repaint: Box<dyn Fn() + Send>) {
    let mut sock: Option<(UdpSocket, u16, [u8; 4])> = None;
    let mut buf = [0u8; 1500];
    loop {
        let c = ctx.lock().unwrap().clone();

        // Luisteren aan/uit, of opnieuw binden na een gewijzigde poort.
        let want = c.auto.enabled.then(|| (c.auto.port, c.auto.bind_addr()));
        if want != sock.as_ref().map(|s| (s.1, s.2)) {
            sock = None;
            let mut st = status.lock().unwrap();
            st.listening = None;
            st.bind_error = None;
            if let Some((port, ip)) = want {
                match UdpSocket::bind(SocketAddr::from((ip, port))) {
                    Ok(s) => {
                        let _ = s.set_read_timeout(Some(Duration::from_millis(250)));
                        st.listening = Some(port);
                        sock = Some((s, port, ip));
                        drop(st);
                        log(&queue, Level::Info, format!("Luistert op UDP-poort {port}"));
                    }
                    Err(e) => {
                        st.bind_error = Some(format!("UDP-poort {port} is niet beschikbaar: {e}"));
                        drop(st);
                        log(&queue, Level::Error, format!("UDP-poort {port} is niet beschikbaar: {e}"));
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            }
            repaint();
        }

        // Een commando ontvangen (of 250 ms wachten).
        match &sock {
            Some((s, ..)) => {
                if let Ok((n, from)) = s.recv_from(&mut buf) {
                    let text = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                    let reply = handle_datagram(&text, from, &c, &status, &queue);
                    if let Some(r) = reply {
                        let _ = s.send_to(r.as_bytes(), from);
                    }
                    repaint();
                }
            }
            None => std::thread::sleep(Duration::from_millis(250)),
        }

        // Handmatige stop, maximale duur, of weggevallen apparaat.
        let running = status.lock().unwrap().running.clone();
        if let Some(r) = running {
            let h = c.engine.clone();
            let still = h.as_ref().is_some_and(|h| h.is_recording());
            let max = Duration::from_secs(c.auto.max_minutes.clamp(1, 240) as u64 * 60);
            if stop_req.swap(false, Ordering::Relaxed) {
                finish(&c, &status, &queue, "handmatig gestopt", Level::Warn);
                repaint();
            } else if !still {
                finish(&c, &status, &queue, "invoerapparaat weggevallen", Level::Error);
                repaint();
            } else if r.since.elapsed() >= max {
                finish(&c, &status, &queue, &format!("geen STOP ontvangen — automatisch gestopt na {} minuten", c.auto.max_minutes), Level::Warn);
                repaint();
            }
        } else {
            stop_req.store(false, Ordering::Relaxed);
        }
    }
}

/// Een binnengekomen datagram afhandelen. Geeft het antwoord terug (None = negeren).
fn handle_datagram(text: &str, from: SocketAddr, c: &Ctx, status: &Arc<Mutex<Status>>, queue: &Arc<Queue>) -> Option<String> {
    if !c.auto.sender_allowed(from.ip()) {
        log(queue, Level::Warn, format!("Genegeerd: \"{text}\" van {} (afzender niet toegestaan)", from.ip()));
        return None;
    }
    let (cmd, key) = match parse(text) {
        Ok(x) => x,
        Err(e) => {
            log(queue, Level::Warn, format!("Onbekend bericht van {}: \"{text}\"", from.ip()));
            return Some(format!("ERR BAD_COMMAND {e}"));
        }
    };
    if !c.auto.key.is_empty() && key.as_deref() != Some(c.auto.key.as_str()) {
        log(queue, Level::Warn, format!("Genegeerd: commando van {} zonder juiste sleutel", from.ip()));
        return Some("ERR BAD_KEY".into());
    }
    let shown = text.split_whitespace().filter(|w| !w.to_ascii_lowercase().starts_with("key=")).collect::<Vec<_>>().join(" ");
    match cmd {
        Command::Status => Some(match status.lock().unwrap().running.clone() {
            Some(r) => format!("OK RECORDING {} {}", r.program_name, mmss(r.since.elapsed().as_secs())),
            None => "OK IDLE".into(),
        }),
        Command::Stop => {
            if status.lock().unwrap().running.is_none() {
                log(queue, Level::Warn, format!("{shown} van {} — er loopt geen automatische opname", from.ip()));
                return Some("ERR NOT_RECORDING".into());
            }
            log(queue, Level::Info, format!("{shown} van {}", from.ip()));
            let secs = finish(c, status, queue, "STOP ontvangen", Level::Info);
            Some(format!("OK STOPPED {}", mmss(secs)))
        }
        Command::Start { program } => {
            let reply = start(c, status, program.as_deref());
            match &reply {
                Ok(name) => log(queue, Level::Ok, format!("{shown} van {} — opname gestart: {name}", from.ip())),
                Err(e) => log(queue, Level::Error, format!("{shown} van {} — niet gestart: {e}", from.ip())),
            }
            Some(match reply {
                Ok(name) => format!("OK RECORDING {name}"),
                Err(e) => format!("ERR {e}"),
            })
        }
    }
}

/// Welk programma? Bericht → rooster → vast (elk aan/uit te zetten).
pub fn resolve_program(c: &Ctx, from_message: Option<&str>, now: chrono::NaiveDateTime) -> Option<(ProgRef, String)> {
    let named = |p: &ProgRef| -> Option<(ProgRef, String)> {
        if let Some(id) = p.id {
            let st = c.studio.as_ref()?;
            let prog = st.programs.iter().find(|x| x.id == id)?;
            return Some((p.clone(), prog.name.clone()));
        }
        let n = p.name.trim();
        (!n.is_empty()).then(|| (p.clone(), n.to_string()))
    };
    if c.auto.use_message {
        if let Some(m) = from_message.map(str::trim).filter(|m| !m.is_empty()) {
            if let Some(st) = &c.studio {
                if let Some(p) = st.programs.iter().find(|p| {
                    p.slug.eq_ignore_ascii_case(m) || p.name.eq_ignore_ascii_case(m) || p.id.to_string() == m
                }) {
                    return Some((ProgRef { id: Some(p.id), name: p.name.clone() }, p.name.clone()));
                }
            }
            if let Some(n) = c.local_programs.iter().find(|n| n.eq_ignore_ascii_case(m)) {
                return Some((ProgRef { id: None, name: n.clone() }, n.clone()));
            }
            // Een genoemd programma dat niet bestaat: niet stilletjes iets anders opnemen.
            return None;
        }
    }
    if c.auto.use_schedule {
        if let Some(r) = slot_at(&c.auto.schedule, now).and_then(|s| named(&s.program)) {
            return Some(r);
        }
    }
    if c.auto.use_fixed {
        if let Some(r) = named(&c.auto.fixed) {
            return Some(r);
        }
    }
    None
}

fn start(c: &Ctx, status: &Mutex<Status>, program: Option<&str>) -> Result<String, String> {
    let Some(h) = &c.engine else { return Err("NO_INPUT Er is geen invoerapparaat open".into()) };
    if h.is_recording() {
        return Err("ALREADY_RECORDING Er loopt al een opname".into());
    }
    let now = Local::now();
    let (prog, name) = resolve_program(c, program, now.naive_local()).ok_or_else(|| {
        match program {
            Some(p) => format!("UNKNOWN_PROGRAM Programma \"{p}\" niet gevonden"),
            None => "UNKNOWN_PROGRAM Geen programma in het rooster en geen vast programma ingesteld".into(),
        }
    })?;
    let folder = PathBuf::from(&c.record_folder);
    std::fs::create_dir_all(&folder).map_err(|e| format!("NO_FOLDER Kan opnamemap niet maken: {e}"))?;
    let base = match safe_file_part(&name) {
        b if b.is_empty() => "Opname".to_string(),
        b => b,
    };
    let path = folder.join(format!("{base}_{}.wav", now.format("%Y-%m-%d_%H-%M-%S")));
    let pa = c.auto.prog(&prog);
    let meta = Meta {
        program: name.clone(),
        program_id: prog.id,
        date: now.format("%Y-%m-%d").to_string(),
        title: title_from(&pa.title_pattern, &name, now),
        description: String::new(),
        speed: 0.0,
    };
    h.start_recording(&path).map_err(|e| format!("START_FAILED {e}"))?;
    meta.save(&path);
    engine::keep_awake(true);
    status.lock().unwrap().running = Some(Running { program: prog, program_name: name.clone(), path, started: now, since: Instant::now() });
    Ok(name)
}

/// Opname stoppen en de verwerking (export, opslaan, upload) op de achtergrond starten.
/// Geeft de opnameduur in seconden terug.
fn finish(c: &Ctx, status: &Arc<Mutex<Status>>, queue: &Arc<Queue>, why: &str, level: Level) -> u64 {
    let Some(r) = status.lock().unwrap().running.take() else { return 0 };
    let res = match &c.engine {
        Some(h) => h.stop_recording(),
        None => engine::RecResult { path: None, frames: 0, peaks: None, error: None },
    };
    engine::keep_awake(false);
    if let Some(e) = res.error {
        log(queue, Level::Error, format!("Opname {}: {e}", r.program_name));
    }
    let secs = r.since.elapsed().as_secs();
    log(queue, level, format!("Opname {} gestopt ({why}), duur {}", r.program_name, mmss(secs)));
    status.lock().unwrap().processing += 1;
    let (c2, st2, q2) = (c.clone(), status.clone(), queue.clone());
    std::thread::Builder::new()
        .name("auto-export".into())
        .spawn(move || {
            let msg = process(&c2, &r, &q2);
            let mut st = st2.lock().unwrap();
            st.processing = st.processing.saturating_sub(1);
            st.last = Some(msg);
        })
        .ok();
    secs
}

fn unique(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let mut dst = dir.join(format!("{name}.{ext}"));
    let mut n = 2;
    while dst.exists() {
        dst = dir.join(format!("{name} ({n}).{ext}"));
        n += 1;
    }
    dst
}

/// Na STOP: exporteren en opslaan en/of uploaden volgens de instellingen van het programma.
/// Het originele WAV-bestand blijft altijd staan.
fn process(c: &Ctx, r: &Running, queue: &Queue) -> (String, bool) {
    let rd = match WavReader::open(&r.path) {
        Ok(rd) => rd,
        Err(e) => {
            let m = format!("Opname {} kon niet worden gelezen: {e} (bestand: {})", r.program_name, r.path.display());
            log(queue, Level::Error, &m);
            return (m, true);
        }
    };
    let (frames, rate) = (rd.frames, rd.fmt.sample_rate.max(1));
    drop(rd);
    if frames == 0 {
        let m = format!("Opname {} is leeg — niets opgeslagen of geüpload", r.program_name);
        log(queue, Level::Error, &m);
        return (m, true);
    }
    let pa = c.auto.prog(&r.program);
    let mut meta = Meta::load(&r.path).unwrap_or_else(|| Meta {
        program: r.program_name.clone(),
        program_id: r.program.id,
        date: r.started.format("%Y-%m-%d").to_string(),
        title: title_from(&pa.title_pattern, &r.program_name, r.started),
        description: String::new(),
        speed: 0.0,
    });
    let studio = c.studio.as_ref();
    let as_prog = r.program.id.and_then(|id| studio.and_then(|s| s.programs.iter().find(|p| p.id == id)));

    // Omschrijving: die van de vorige aflevering (AudioShare) of de vaste tekst.
    meta.description = pa.description.trim().to_string();
    if pa.previous_description {
        if let (Some(st), Some(p)) = (studio, as_prog) {
            let tok = queue.tokens.lock().unwrap().get(&st.profile_id).map(|t| t.1.clone());
            if let Some(tok) = tok {
                if let Ok(info) = Client::new(&st.url, Some(tok)).upload_info(p.id) {
                    if !info.suggested_description.trim().is_empty() {
                        meta.description = info.suggested_description.trim().to_string();
                    }
                }
            }
        }
    }
    meta.save(&r.path);

    // Bestandsnaam = de titel (die bevat standaard al programma en datum).
    let name = match safe_file_part(&meta.title) {
        n if n.is_empty() => meta.export_name().unwrap_or_else(|| safe_file_part(&r.program_name)),
        n => n,
    };
    let tags = meta.tags(&c.studio_name);
    let segments = vec![(0, frames)];
    let export_to = |dst: &Path, format: ExportFormat| -> Result<(), String> {
        let job = ExportJob {
            src: &r.path,
            dst,
            format,
            segments: segments.clone(),
            fade_ms: c.fade_ms,
            crossfade_ms: 10,
            tags: tags.clone(),
            opts: export::ExportOptions::default(),
        };
        export::export(&job, &AtomicU32::new(0), &AtomicBool::new(false))
    };

    let mut done = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let auto_dir = PathBuf::from(&c.record_folder).join("Automatisch").join(safe_file_part(&r.program_name));

    if pa.after_stop.saves() {
        let dir = if pa.folder.trim().is_empty() { auto_dir.clone() } else { PathBuf::from(pa.folder.trim()) };
        let fname = match safe_file_part(&title_from(&pa.file_pattern, &r.program_name, r.started)) {
            n if pa.file_pattern.trim().is_empty() || n.is_empty() => name.clone(),
            n => n,
        };
        let ext = pa.save_format.ext();
        let res = std::fs::create_dir_all(&dir).map_err(|e| format!("map {}: {e}", dir.display())).and_then(|_| {
            if pa.overwrite {
                // Eerst volledig naar een tijdelijk bestand, dan in één keer omwisselen:
                // wie het bestand leest (playout) ziet nooit een half bestand.
                let dst = dir.join(format!("{fname}.{ext}"));
                let tmp = dir.join(format!("~{fname}.tmp.{ext}"));
                export_to(&tmp, pa.save_format)?;
                match std::fs::rename(&tmp, &dst) {
                    Ok(()) => Ok((dst, true)),
                    Err(e) => {
                        // Oud bestand is in gebruik: nieuwe versie toch bewaren, met een andere naam.
                        let alt = unique(&dir, &fname, ext);
                        let _ = std::fs::rename(&tmp, &alt);
                        Err(format!("{} kon niet worden overschreven ({e}); nieuwe opname staat in {}", dst.display(), alt.display()))
                    }
                }
            } else {
                let dst = unique(&dir, &fname, ext);
                export_to(&dst, pa.save_format).map(|_| (dst, false))
            }
        });
        let fmt = match pa.save_format {
            ExportFormat::Flac => "FLAC",
            ExportFormat::Wav => "WAV",
            ExportFormat::Mp3 => "MP3",
        };
        match res {
            Ok((dst, replaced)) => {
                log(queue, Level::Ok, format!("Opgeslagen als {fmt}{}: {}", if replaced { " (overschreven)" } else { "" }, dst.display()));
                done.push(format!("opgeslagen als {fmt}: {}", dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            }
            Err(e) => {
                log(queue, Level::Error, format!("Opslaan {} mislukt: {e}", r.program_name));
                errors.push(format!("opslaan mislukt: {e}"));
            }
        }
    }

    if pa.after_stop.uploads() {
        match (studio, as_prog) {
            (None, _) => {
                log(queue, Level::Error, format!("{}: niet geüpload — deze pc is niet als studio gekoppeld met AudioShare", r.program_name));
                errors.push("niet geüpload: deze pc is niet gekoppeld met AudioShare".into());
            }
            (Some(_), None) => {
                log(queue, Level::Error, format!("{}: niet geüpload — dit is geen AudioShare-programma van deze studio", r.program_name));
                errors.push("niet geüpload: geen AudioShare-programma van deze studio".into());
            }
            (Some(st), Some(p)) => {
                let dir = auto_dir.join("upload");
                let res = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|_| {
                    let dst = unique(&dir, &name, ExportFormat::Mp3.ext());
                    export_to(&dst, ExportFormat::Mp3).map(|_| dst)
                });
                match res {
                    Ok(dst) => {
                        queue.add(Job {
                            profile_id: st.profile_id.clone(),
                            profile_name: st.profile_name.clone(),
                            operator: LOG.into(),
                            program_id: p.id,
                            program_name: p.name.clone(),
                            date: meta.date.clone(),
                            title: meta.title.clone(),
                            description: meta.description.clone(),
                            file: dst,
                            duration_sec: frames as f64 / rate as f64,
                            sample_rate: rate,
                            quality: p.quality.clone(),
                            ..Default::default()
                        });
                        done.push("in de uploadwachtrij".to_string());
                    }
                    Err(e) => {
                        log(queue, Level::Error, format!("Export voor upload {} mislukt: {e}", r.program_name));
                        errors.push(format!("upload-export mislukt: {e}"));
                    }
                }
            }
        }
    }

    let mut parts = done;
    parts.extend(errors.iter().cloned());
    let msg = if parts.is_empty() { format!("{} — niets te doen", r.program_name) } else { format!("{} — {}", r.program_name, parts.join(" · ")) };
    (msg, !errors.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }

    #[test]
    fn parse_commands() {
        assert_eq!(parse("START").unwrap(), (Command::Start { program: None }, None));
        assert_eq!(parse("  start \r\n").unwrap().0, Command::Start { program: None });
        assert_eq!(parse("START DeBlik1").unwrap().0, Command::Start { program: Some("DeBlik1".into()) });
        assert_eq!(parse("STOP key=geheim").unwrap(), (Command::Stop, Some("geheim".into())));
        assert_eq!(parse("key=x START De Blik").unwrap(), (Command::Start { program: Some("De Blik".into()) }, Some("x".into())));
        assert_eq!(parse("status").unwrap().0, Command::Status);
        assert!(parse("").is_err());
        assert!(parse("RECORD").is_err());
    }

    #[test]
    fn senders() {
        let s = AutoSettings::default();
        assert!(s.sender_allowed("127.0.0.1".parse().unwrap()));
        assert!(!s.sender_allowed("192.168.1.20".parse().unwrap()));
        assert_eq!(s.bind_addr(), [127, 0, 0, 1]);
        let s = AutoSettings { allowed: vec!["127.0.0.1".into(), "192.168.1.20".into()], ..Default::default() };
        assert!(s.sender_allowed("192.168.1.20".parse().unwrap()));
        assert_eq!(s.bind_addr(), [0, 0, 0, 0]);
        let s = AutoSettings { allowed: vec!["*".into()], ..Default::default() };
        assert!(s.sender_allowed("10.0.0.9".parse().unwrap()));
    }

    #[test]
    fn schedule() {
        // vrijdag 20 november 2026
        let sch = vec![
            Slot { days: vec![1, 2, 3, 4, 5], from: "09:00".into(), to: "10:00".into(), program: ProgRef { id: None, name: "Ochtend".into() } },
            Slot { days: vec![5], from: "23:00".into(), to: "01:00".into(), program: ProgRef { id: None, name: "Nacht".into() } },
        ];
        assert_eq!(slot_at(&sch, at(2026, 11, 20, 9, 30)).unwrap().program.name, "Ochtend");
        assert_eq!(slot_at(&sch, at(2026, 11, 20, 8, 56)).unwrap().program.name, "Ochtend", "5 min speling vooraf");
        assert!(slot_at(&sch, at(2026, 11, 20, 8, 50)).is_none());
        assert!(slot_at(&sch, at(2026, 11, 20, 10, 0)).is_none(), "eindtijd telt niet meer mee");
        assert!(slot_at(&sch, at(2026, 11, 21, 9, 30)).is_none(), "zaterdag");
        assert_eq!(slot_at(&sch, at(2026, 11, 20, 23, 30)).unwrap().program.name, "Nacht");
        assert_eq!(slot_at(&sch, at(2026, 11, 21, 0, 30)).unwrap().program.name, "Nacht", "na middernacht, blok van vrijdag");
        assert!(slot_at(&sch, at(2026, 11, 22, 0, 30)).is_none(), "zondagnacht: blok van zaterdag bestaat niet");
    }

    #[test]
    fn program_choice_order() {
        let mut c = Ctx { local_programs: vec!["Ochtend".into(), "Vast".into()], ..Default::default() };
        c.auto.schedule = vec![Slot { days: vec![5], from: "09:00".into(), to: "10:00".into(), program: ProgRef { id: None, name: "Ochtend".into() } }];
        c.auto.fixed = ProgRef { id: None, name: "Vast".into() };
        let in_slot = at(2026, 11, 20, 9, 30);
        let outside = at(2026, 11, 20, 14, 0);
        assert_eq!(resolve_program(&c, None, in_slot).unwrap().1, "Ochtend");
        assert_eq!(resolve_program(&c, None, outside).unwrap().1, "Vast");
        assert_eq!(resolve_program(&c, Some("vast"), in_slot).unwrap().1, "Vast", "bericht gaat voor");
        assert!(resolve_program(&c, Some("Bestaat niet"), in_slot).is_none(), "onbekend genoemd programma: niet opnemen");
        c.auto.use_schedule = false;
        assert_eq!(resolve_program(&c, None, in_slot).unwrap().1, "Vast");
        c.auto.use_fixed = false;
        assert!(resolve_program(&c, None, in_slot).is_none());
        // AudioShare-programma van de studio, op slug
        c.studio = Some(StudioLink {
            programs: vec![Program { id: 4, slug: "DeBlik1".into(), name: "De Blik uur 1".into(), ..Default::default() }],
            ..Default::default()
        });
        let (p, n) = resolve_program(&c, Some("deblik1"), in_slot).unwrap();
        assert_eq!((p.id, n.as_str()), (Some(4), "De Blik uur 1"));
    }

    #[test]
    fn titles() {
        use chrono::TimeZone;
        let when = Local.with_ymd_and_hms(2026, 11, 20, 21, 5, 0).unwrap();
        assert_eq!(title_from("{programma} — {datum}", "De Blik", when), "De Blik — 20 november");
        assert_eq!(title_from("", "De Blik", when), "De Blik — 20 november");
        assert_eq!(title_from("{programma} {tijd} week {week}", "X", when), "X 21:05 week 47");
    }
}

/// End-to-end over echte UDP met de demo-invoer: START, STATUS, STOP en de
/// verwerking (opslaan als WAV in een tijdelijke map).
#[cfg(test)]
mod udp_tests {
    use super::*;

    fn send(sock: &UdpSocket, port: u16, msg: &str) -> String {
        sock.send_to(msg.as_bytes(), ("127.0.0.1", port)).unwrap();
        let mut b = [0u8; 512];
        let (n, _) = sock.recv_from(&mut b).expect("antwoord");
        String::from_utf8_lossy(&b[..n]).to_string()
    }

    #[test]
    fn start_status_stop_over_udp() {
        let dir = std::env::temp_dir().join(format!("autotest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Eigen config-map, zodat uploads.json niet in de echte gebruikersmap komt.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.join("cfg")) };
        unsafe { std::env::set_var("APPDATA", dir.join("cfg")) };
        let queue = Queue::start();
        let engine = crate::engine::Engine::open(&crate::engine::InputChoice { id: crate::engine::DEMO_ID.into(), name: "demo".into(), loopback: false }).expect("demo-invoer");
        let port = 47900 + (std::process::id() % 50) as u16;
        let auto = Auto::start(queue.clone(), || {});
        {
            let mut c = auto.ctx.lock().unwrap();
            c.engine = Some(engine.handle());
            c.record_folder = dir.to_string_lossy().into_owned();
            c.fade_ms = 10;
            c.local_programs = vec!["Testprogramma".into()];
            c.auto.enabled = true;
            c.auto.port = port;
            c.auto.fixed = ProgRef { id: None, name: "Testprogramma".into() };
            c.auto.programs.insert(
                "Testprogramma".into(),
                ProgAuto { after_stop: AfterStop::Save, save_format: ExportFormat::Wav, ..Default::default() },
            );
        }
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        for _ in 0..40 {
            if auto.status.lock().unwrap().listening.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(send(&client, port, "STOP"), "ERR NOT_RECORDING");
        assert_eq!(send(&client, port, "START"), "OK RECORDING Testprogramma");
        assert!(send(&client, port, "START").starts_with("ERR ALREADY_RECORDING"));
        std::thread::sleep(Duration::from_millis(1500));
        assert!(send(&client, port, "STATUS").starts_with("OK RECORDING Testprogramma 00:00:0"));
        assert!(send(&client, port, "STOP").starts_with("OK STOPPED 00:00:0"));
        assert_eq!(send(&client, port, "STATUS"), "OK IDLE");
        // wachten op de verwerking
        for _ in 0..100 {
            let st = auto.status.lock().unwrap();
            if st.processing == 0 && st.last.is_some() {
                break;
            }
            drop(st);
            std::thread::sleep(Duration::from_millis(100));
        }
        let last = auto.status.lock().unwrap().last.clone().unwrap();
        assert!(!last.1, "verwerking mislukt: {}", last.0);
        let saved: Vec<_> = std::fs::read_dir(dir.join("Automatisch").join("Testprogramma")).unwrap().flatten().collect();
        assert_eq!(saved.len(), 1, "één opgeslagen bestand");
        let name = saved[0].file_name().to_string_lossy().into_owned();
        assert!(name.starts_with("Testprogramma — ") && name.ends_with(".wav"), "{name}");
        // origineel blijft staan
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".wav")));
        // afzender niet toegestaan → geen antwoord
        auto.ctx.lock().unwrap().auto.allowed = vec!["10.9.9.9".into()];
        std::thread::sleep(Duration::from_millis(600));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Vaste bestandsnaam + overschrijven: steeds hetzelfde bestand, zonder (2).
#[cfg(test)]
mod overwrite_tests {
    use super::*;

    #[test]
    fn fixed_name_overwrites() {
        let dir = std::env::temp_dir().join(format!("autoow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.join("cfg")) };
        unsafe { std::env::set_var("APPDATA", dir.join("cfg")) };
        let queue = Queue::start();
        // een korte opname maken
        let wav = dir.join("bron.wav");
        let mut w = crate::wav::WavWriter::create(&wav, 48000, 2, 24).unwrap();
        w.write_f32(&vec![0.1f32; 48000 * 2]).unwrap();
        w.finalize().unwrap();
        let mut c = Ctx { record_folder: dir.to_string_lossy().into_owned(), fade_ms: 10, ..Default::default() };
        let prog = ProgRef { id: None, name: "Testprogramma".into() };
        c.auto.programs.insert(
            prog.key(),
            ProgAuto { save_format: ExportFormat::Wav, file_pattern: "testprogramma".into(), overwrite: true, ..Default::default() },
        );
        let r = Running { program: prog, program_name: "Testprogramma".into(), path: wav, started: Local::now(), since: Instant::now() };
        for _ in 0..2 {
            let (msg, err) = process(&c, &r, &queue);
            assert!(!err, "{msg}");
        }
        let out = dir.join("Automatisch").join("Testprogramma");
        let names: Vec<String> = std::fs::read_dir(&out).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["testprogramma.wav".to_string()], "precies één bestand, overschreven");
        // zonder overschrijven: (2)
        c.auto.programs.get_mut("Testprogramma").unwrap().overwrite = false;
        process(&c, &r, &queue);
        assert!(out.join("testprogramma (2).wav").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
