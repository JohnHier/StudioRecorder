//! Upload queue (spec §6): survives restarts, retries temporary problems,
//! never creates duplicates (same client_upload_id on every retry).

use super::api::{ApiError, ApiWarning, Client, Quality, sha256_file};
use super::profiles::now;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    /// Waiting for its turn (or for the maker to unlock their profile).
    Waiting,
    Creating,
    Uploading,
    Finalizing,
    /// An own episode already exists on that date: the user must confirm.
    NeedsConfirm,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_active(self) -> bool {
        matches!(self, JobState::Waiting | JobState::Creating | JobState::Uploading | JobState::Finalizing)
    }
    pub fn is_open(self) -> bool {
        self.is_active() || self == JobState::NeedsConfirm
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    /// = client_upload_id
    pub id: String,
    pub profile_id: String,
    pub profile_name: String,
    pub program_id: i64,
    pub program_name: String,
    pub date: String,
    pub title: String,
    pub description: String,
    pub file: PathBuf,
    pub duration_sec: f64,
    pub sample_rate: u32,
    pub quality: Quality,
    pub created_at: String,
    pub state: JobState,
    pub draft_id: Option<i64>,
    pub episode_id: Option<i64>,
    /// Episode to replace (own episode on the same date).
    pub replace_episode: Option<i64>,
    pub confirm_replace: bool,
    /// Details from CONFIRM_REQUIRED, for the warning dialog.
    pub confirm_details: Value,
    pub sha256: String,
    pub attempt: u32,
    pub next_try: i64,
    pub message: String,
    pub revision: Option<i64>,
    pub warnings: Vec<ApiWarning>,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: String::new(),
            profile_name: String::new(),
            program_id: 0,
            program_name: String::new(),
            date: String::new(),
            title: String::new(),
            description: String::new(),
            file: PathBuf::new(),
            duration_sec: 0.0,
            sample_rate: 0,
            quality: Quality::default(),
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            state: JobState::Waiting,
            draft_id: None,
            episode_id: None,
            replace_episode: None,
            confirm_replace: false,
            confirm_details: Value::Null,
            sha256: String::new(),
            attempt: 0,
            next_try: 0,
            message: String::new(),
            revision: None,
            warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub time: String,
    pub profile: String,
    pub level: Level,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueData {
    pub jobs: Vec<Job>,
    pub log: Vec<LogEntry>,
}

fn path() -> PathBuf {
    crate::settings::config_dir().join("uploads.json")
}

impl QueueData {
    fn load() -> QueueData {
        let mut q: QueueData = std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        // An interrupted step is simply retried.
        for j in &mut q.jobs {
            if j.state.is_active() {
                j.next_try = 0;
            }
        }
        q
    }

    fn save(&self) {
        crate::settings::write_json(&path(), self);
    }

    pub fn add_log(&mut self, profile: &str, level: Level, text: impl Into<String>) {
        self.log.push(LogEntry {
            time: chrono::Local::now().format("%d-%m %H:%M:%S").to_string(),
            profile: profile.to_string(),
            level,
            text: text.into(),
        });
        let n = self.log.len();
        if n > 1000 {
            self.log.drain(0..n - 1000);
        }
    }
}

/// Retry schedule from spec §6: 10 s, 30 s, 1 min, 5 min, then every 5 min.
pub fn backoff(attempt: u32) -> i64 {
    match attempt {
        0 | 1 => 10,
        2 => 30,
        3 => 60,
        _ => 300,
    }
}

/// Local check against the programme's norms before anything is sent (§6 step 1).
pub fn precheck(file: &std::path::Path, duration_sec: f64, sample_rate: u32, q: &Quality) -> Vec<String> {
    let mut problems = Vec::new();
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let formats: Vec<String> = if q.formats.is_empty() {
        vec!["mp3".into(), "flac".into(), "ogg".into()]
    } else {
        q.formats.iter().map(|f| f.to_ascii_lowercase()).collect()
    };
    if !formats.contains(&ext) {
        problems.push(format!("Formaat {} wordt niet geaccepteerd (wel: {})", ext.to_uppercase(), formats.join(", ").to_uppercase()));
    }
    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let max_mb = q.max_file_mb.unwrap_or(200) as u64;
    if size > max_mb * 1024 * 1024 {
        problems.push(format!("Bestand is {} MB, maximaal {} MB", size / (1024 * 1024), max_mb));
    }
    if let Some(min_mb) = q.min_file_mb {
        if size < min_mb as u64 * 1024 * 1024 {
            problems.push(format!("Bestand is {:.1} MB, minimaal {} MB", size as f64 / 1_048_576.0, min_mb));
        }
    }
    if !q.samplerates.is_empty() && sample_rate > 0 && !q.samplerates.contains(&sample_rate) {
        problems.push(format!(
            "Samplerate {} Hz wordt niet geaccepteerd (wel: {})",
            sample_rate,
            q.samplerates.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(min) = q.min_duration_sec {
        if duration_sec + 0.5 < min as f64 {
            problems.push(format!("Duur {} is korter dan het minimum van {}", mmss(duration_sec), mmss(min as f64)));
        }
    }
    if let Some(max) = q.max_duration_sec {
        if duration_sec > max as f64 + 0.5 {
            problems.push(format!("Duur {} is langer dan het maximum van {}", mmss(duration_sec), mmss(max as f64)));
        }
    }
    if let (Some(min), true) = (q.min_bitrate_kbps, duration_sec > 1.0) {
        let kbps = size as f64 * 8.0 / duration_sec / 1000.0;
        if kbps + 2.0 < min as f64 {
            problems.push(format!("Bitrate {:.0} kbps is lager dan het minimum van {} kbps", kbps, min));
        }
    }
    problems
}

pub fn mmss(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Shared between the UI and the worker thread.
pub struct Queue {
    pub data: Mutex<QueueData>,
    /// Tokens the worker may use, per profile id: (url, token).
    pub tokens: Mutex<HashMap<String, (String, String)>>,
    /// Bytes sent / total for the job being uploaded.
    pub progress: Mutex<Option<(String, Arc<AtomicU64>, u64)>>,
    /// Profiles whose token AudioShare rejected (401), for the UI to handle.
    pub unlinked: Mutex<Vec<String>>,
    wake: AtomicBool,
}

impl Queue {
    pub fn start() -> Arc<Queue> {
        let q = Arc::new(Queue {
            data: Mutex::new(QueueData::load()),
            tokens: Mutex::new(HashMap::new()),
            progress: Mutex::new(None),
            unlinked: Mutex::new(Vec::new()),
            wake: AtomicBool::new(false),
        });
        let w = q.clone();
        std::thread::Builder::new().name("upload-queue".into()).spawn(move || worker(w)).expect("thread");
        q
    }

    pub fn wake(&self) {
        self.wake.store(true, Ordering::Relaxed);
    }

    pub fn add(&self, job: Job) {
        let mut d = self.data.lock().unwrap();
        d.add_log(&job.profile_name, Level::Info, format!("In wachtrij: {} — {}", job.program_name, job.title));
        d.jobs.push(job);
        d.save();
        drop(d);
        self.wake();
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut QueueData) -> R) -> R {
        let mut d = self.data.lock().unwrap();
        let r = f(&mut d);
        d.save();
        drop(d);
        self.wake();
        r
    }

    /// Does this profile still have uploads that need its token?
    pub fn has_open_jobs(&self, profile_id: &str) -> bool {
        self.data.lock().unwrap().jobs.iter().any(|j| j.profile_id == profile_id && j.state.is_open())
    }

    /// User confirmed replacing (dialog in §5).
    pub fn confirm_replace(&self, job_id: &str) {
        self.with(|d| {
            if let Some(j) = d.jobs.iter_mut().find(|j| j.id == job_id) {
                j.confirm_replace = true;
                j.state = JobState::Creating;
                j.next_try = 0;
                j.message = "Vervangen bevestigd".into();
            }
        });
    }

    pub fn cancel(&self, job_id: &str) {
        self.with(|d| {
            let mut log = None;
            if let Some(j) = d.jobs.iter_mut().find(|j| j.id == job_id) {
                if j.state != JobState::Done {
                    j.state = JobState::Cancelled;
                    j.message = "Geannuleerd".into();
                    log = Some((j.profile_name.clone(), format!("Geannuleerd: {} — {}", j.program_name, j.title)));
                }
            }
            if let Some((p, t)) = log {
                d.add_log(&p, Level::Warn, t);
            }
        });
    }

    pub fn retry(&self, job_id: &str) {
        self.with(|d| {
            if let Some(j) = d.jobs.iter_mut().find(|j| j.id == job_id) {
                j.state = if j.draft_id.is_some() { JobState::Uploading } else { JobState::Waiting };
                j.attempt = 0;
                j.next_try = 0;
                j.message = "Opnieuw in de wachtrij".into();
            }
        });
    }

    pub fn remove_finished(&self) {
        self.with(|d| d.jobs.retain(|j| j.state.is_open()));
    }
}

enum Step {
    /// Go on with the next state right away.
    Next,
    /// Stop working on this job for now.
    Pause,
}

fn worker(q: Arc<Queue>) {
    loop {
        // Pick the first job that can run now.
        let t = now();
        let pick = {
            let d = q.data.lock().unwrap();
            let tokens = q.tokens.lock().unwrap();
            d.jobs
                .iter()
                .find(|j| j.state.is_active() && j.next_try <= t && tokens.contains_key(&j.profile_id))
                .map(|j| (j.clone(), tokens.get(&j.profile_id).cloned().unwrap()))
        };
        let Some((mut job, (url, token))) = pick else {
            for _ in 0..10 {
                if q.wake.swap(false, Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            continue;
        };
        let client = Client::new(&url, Some(token));
        loop {
            let step = run_step(&q, &client, &mut job);
            // Write back (the UI may have cancelled in the meantime).
            let cancelled = {
                let mut d = q.data.lock().unwrap();
                let mut cancelled = false;
                if let Some(slot) = d.jobs.iter_mut().find(|j| j.id == job.id) {
                    if slot.state == JobState::Cancelled {
                        cancelled = true;
                    } else {
                        *slot = job.clone();
                    }
                }
                d.save();
                cancelled
            };
            if cancelled || matches!(step, Step::Pause) || !job.state.is_active() {
                break;
            }
        }
    }
}

fn log(q: &Queue, job: &Job, level: Level, text: impl Into<String>) {
    q.data.lock().unwrap().add_log(&job.profile_name, level, text);
}

/// Handle an API error: retry later, or give up with a message.
fn fail(q: &Queue, job: &mut Job, e: ApiError) -> Step {
    if e.is_temporary() {
        job.attempt += 1;
        let wait = backoff(job.attempt);
        job.next_try = now() + wait;
        let at = chrono::Local::now() + chrono::Duration::seconds(wait);
        job.message = format!("{} — opnieuw proberen om {}", e.user_message(), at.format("%H:%M:%S"));
        if job.attempt == 1 || job.attempt % 5 == 0 {
            log(q, job, Level::Warn, format!("{} — {}: {}", job.program_name, job.title, job.message));
        }
        return Step::Pause;
    }
    if e.code() == "INVALID_TOKEN" {
        q.unlinked.lock().unwrap().push(job.profile_id.clone());
        q.tokens.lock().unwrap().remove(&job.profile_id);
        // Stays in the queue: after pairing again it continues.
        job.message = e.user_message();
        job.next_try = now() + 30;
        log(q, job, Level::Error, e.user_message());
        return Step::Pause;
    }
    job.state = JobState::Failed;
    job.message = e.user_message();
    log(q, job, Level::Error, format!("Upload {} — {} mislukt: {}", job.program_name, job.title, job.message));
    Step::Pause
}

fn run_step(q: &Queue, c: &Client, job: &mut Job) -> Step {
    match job.state {
        JobState::Waiting => {
            if !job.file.exists() {
                job.state = JobState::Failed;
                job.message = format!("Bestand niet gevonden: {}", job.file.display());
                log(q, job, Level::Error, job.message.clone());
                return Step::Pause;
            }
            let problems = precheck(&job.file, job.duration_sec, job.sample_rate, &job.quality);
            if !problems.is_empty() {
                job.state = JobState::Failed;
                job.message = format!("Voldoet niet aan de normen: {}", problems.join("; "));
                log(q, job, Level::Error, format!("{} — {}: {}", job.program_name, job.title, job.message));
                return Step::Pause;
            }
            job.message = "Controlegetal berekenen…".into();
            match sha256_file(&job.file) {
                Ok(h) => job.sha256 = h,
                Err(e) => {
                    job.state = JobState::Failed;
                    job.message = format!("Bestand niet leesbaar: {e}");
                    return Step::Pause;
                }
            }
            job.state = JobState::Creating;
            Step::Next
        }
        JobState::Creating => {
            job.message = "Aflevering aanmaken…".into();
            let res = match (job.replace_episode, job.confirm_replace) {
                (Some(ep), true) => c.replace(ep, &job.id, &job.title, &job.description, true),
                _ => c.create_episode(job.program_id, &job.id, &job.date, &job.title, &job.description),
            };
            match res {
                Ok(d) => {
                    job.draft_id = Some(d.draft_id);
                    job.attempt = 0;
                    job.state = JobState::Uploading;
                    Step::Next
                }
                Err(e) if e.code() == "EPISODE_EXISTS" => {
                    let det = e.details();
                    let is_own = det.get("is_own").and_then(Value::as_bool).unwrap_or(false);
                    let owner = det.get("owner").and_then(Value::as_str).unwrap_or("een andere maker").to_string();
                    let ep = det.get("episode_id").and_then(Value::as_i64);
                    if !is_own || ep.is_none() {
                        job.state = JobState::Failed;
                        job.message = format!(
                            "Op deze datum staat al een aflevering van {owner}. Kies een andere datum of neem contact op met {owner} of de beheerder."
                        );
                        log(q, job, Level::Error, format!("{} — {}: {}", job.program_name, job.title, job.message));
                        return Step::Pause;
                    }
                    job.replace_episode = ep;
                    // Ask for the warning details (always CONFIRM_REQUIRED without confirm).
                    match c.replace(ep.unwrap(), &job.id, &job.title, &job.description, false) {
                        Err(e2) if e2.code() == "CONFIRM_REQUIRED" => {
                            job.confirm_details = e2.details().clone();
                            job.state = JobState::NeedsConfirm;
                            job.message = "Er staat al een aflevering op deze datum — bevestig of je wilt vervangen".into();
                            log(q, job, Level::Warn, format!("{} — {}: wacht op bevestiging om te vervangen", job.program_name, job.title));
                            Step::Pause
                        }
                        Ok(d) => {
                            job.draft_id = Some(d.draft_id);
                            job.state = JobState::Uploading;
                            Step::Next
                        }
                        Err(e2) => fail(q, job, e2),
                    }
                }
                Err(e) => fail(q, job, e),
            }
        }
        JobState::Uploading => {
            let Some(draft) = job.draft_id else {
                job.state = JobState::Creating;
                return Step::Next;
            };
            let total = std::fs::metadata(&job.file).map(|m| m.len()).unwrap_or(0);
            let sent = Arc::new(AtomicU64::new(0));
            *q.progress.lock().unwrap() = Some((job.id.clone(), sent.clone(), total));
            job.message = "Uploaden…".into();
            let res = c.put_part(draft, 1, &job.file, &job.sha256, sent);
            *q.progress.lock().unwrap() = None;
            match res {
                Ok(_) => {
                    job.attempt = 0;
                    job.state = JobState::Finalizing;
                    Step::Next
                }
                Err(e) if e.code() == "ALREADY_FINALIZED" => {
                    job.state = JobState::Finalizing;
                    Step::Next
                }
                Err(e) if e.code() == "CHECKSUM_MISMATCH" && job.attempt < 3 => {
                    job.attempt += 1;
                    job.sha256 = sha256_file(&job.file).unwrap_or_default();
                    Step::Next
                }
                Err(e) => fail(q, job, e),
            }
        }
        JobState::Finalizing => {
            let Some(draft) = job.draft_id else {
                job.state = JobState::Creating;
                return Step::Next;
            };
            job.message = "Vrijgeven…".into();
            match c.finalize(draft) {
                Ok(f) => {
                    job.episode_id = Some(f.episode_id);
                    job.revision = Some(f.revision);
                    job.warnings = f.warnings.clone();
                    job.state = JobState::Done;
                    job.message = if f.replaced { format!("Vervangen (revisie {})", f.revision) } else { "Gepubliceerd".into() };
                    let what = if f.replaced { format!("Vervangen (revisie {})", f.revision) } else { "gelukt".into() };
                    log(q, job, Level::Ok, format!("Upload {} — {}: {}", job.program_name, job.title, what));
                    for w in &f.warnings {
                        log(q, job, Level::Warn, w.message.clone());
                    }
                    Step::Pause
                }
                Err(e) if e.code() == "PARTS_MISSING" => {
                    job.state = JobState::Uploading;
                    Step::Next
                }
                Err(e) => fail(q, job, e),
            }
        }
        _ => Step::Pause,
    }
}
