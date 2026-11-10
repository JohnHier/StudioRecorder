//! HTTP client for the AudioShare Maker API (koppelspecificatie v0.5).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// An error from the API, or a network problem.
#[derive(Clone, Debug)]
pub enum ApiError {
    /// No answer (network, DNS, timeout, TLS). Worth retrying.
    Network(String),
    /// The server answered with an error.
    Http { status: u16, code: String, message: String, details: Value },
}

impl ApiError {
    pub fn code(&self) -> &str {
        match self {
            ApiError::Network(_) => "NETWORK",
            ApiError::Http { code, .. } => code,
        }
    }
    #[allow(dead_code)]
    pub fn status(&self) -> u16 {
        match self {
            ApiError::Network(_) => 0,
            ApiError::Http { status, .. } => *status,
        }
    }
    /// Network problems and 5xx are temporary; 4xx are not (spec §6).
    pub fn is_temporary(&self) -> bool {
        match self {
            ApiError::Network(_) => true,
            ApiError::Http { status, .. } => *status >= 500,
        }
    }
    pub fn details(&self) -> &Value {
        static NULL: Value = Value::Null;
        match self {
            ApiError::Http { details, .. } => details,
            _ => &NULL,
        }
    }
    /// A Dutch message for the user.
    pub fn user_message(&self) -> String {
        match self {
            ApiError::Network(m) => format!("Geen verbinding met AudioShare ({m})"),
            ApiError::Http { status, code, message, details } => match code.as_str() {
                "INVALID_TOKEN" => "Deze recorder is ontkoppeld — vraag je beheerder om een nieuwe koppelcode.".into(),
                "INVALID_CLAIM_CODE" => "Deze koppelcode bestaat niet of is ingetrokken.".into(),
                "CLAIM_CODE_USED" => "Deze koppelcode is al gebruikt. Vraag je beheerder om een nieuwe.".into(),
                "CLAIM_CODE_EXPIRED" => "Deze koppelcode is verlopen. Vraag je beheerder om een nieuwe.".into(),
                "TOO_MANY_ATTEMPTS" => "Te veel pogingen. Probeer het over een uur opnieuw.".into(),
                "NOT_ASSIGNED" => "Dit programma is niet (meer) aan jou toegewezen.".into(),
                "NO_PERMISSION" => "Je hebt geen toestemming (meer) om programma's aan te leveren. Neem contact op met de beheerder.".into(),
                "NOT_OWNER" => {
                    let owner = details.get("owner").and_then(Value::as_str).unwrap_or("een andere maker");
                    format!("Deze aflevering is van {owner} — neem contact op met {owner} of de beheerder.")
                }
                "FILE_TOO_LARGE" => "Het bestand is groter dan 200 MB.".into(),
                "INVALID_BROADCAST_DATE" => "De uitzenddatum ligt in het verleden.".into(),
                _ if !message.is_empty() => message.clone(),
                _ => format!("AudioShare gaf fout {status} ({code})"),
            },
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: i64,
    #[serde(default)]
    pub username: String,
    pub display_name: String,
    /// "admin" or a normal role (not in spec v0.5, sent by AudioShare).
    #[serde(default)]
    pub role: String,
    /// "studio" for a studio recorder (AudioShare 1.5.0), otherwise "person".
    #[serde(default)]
    pub account_type: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ClaimResponse {
    pub token: String,
    pub user: UserInfo,
    #[serde(default)]
    #[allow(dead_code)]
    pub device_id: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MeResponse {
    pub user: UserInfo,
    #[serde(default)]
    #[allow(dead_code)]
    pub permissions: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quality {
    pub formats: Vec<String>,
    pub min_bitrate_kbps: Option<u32>,
    pub samplerates: Vec<u32>,
    pub min_duration_sec: Option<u32>,
    pub max_duration_sec: Option<u32>,
    pub max_file_mb: Option<u32>,
    /// Not in spec v0.5, but sent by AudioShare (e.g. 20).
    pub min_file_mb: Option<u32>,
    pub lufs_min: Option<f64>,
    pub lufs_max: Option<f64>,
    pub true_peak_max: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Program {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub quality: Quality,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UploadInfo {
    pub suggested_title: String,
    pub suggested_description: String,
    pub last_broadcast_date: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub draft_id: i64,
    pub kind: String,
    pub episode_id: Option<i64>,
    pub status: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiWarning {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FinalizeResponse {
    pub episode_id: i64,
    pub status: String,
    pub replaced: bool,
    pub revision: i64,
    pub warnings: Vec<ApiWarning>,
    pub already_finalized: bool,
}

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
}

fn agent(timeout: Duration) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    let tls = TlsConfig::builder().provider(TlsProvider::NativeTls).root_certs(RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .tls_config(tls)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(timeout))
        .user_agent(concat!("StudioRecorder/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Normalise the platform address ("platform.br6.nl/" → "https://platform.br6.nl").
pub fn normalize_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.starts_with("http://") || u.starts_with("https://") { u.to_string() } else { format!("https://{u}") }
}

impl Client {
    pub fn new(base: &str, token: Option<String>) -> Self {
        Client { base: normalize_url(base), token, agent: agent(Duration::from_secs(60)) }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token.as_deref().unwrap_or(""))
    }

    fn handle<T: serde::de::DeserializeOwned>(r: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<T, ApiError> {
        let mut resp = r.map_err(|e| ApiError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().with_config().limit(10 * 1024 * 1024).read_to_string().map_err(|e| ApiError::Network(e.to_string()))?;
        if (200..300).contains(&status) {
            let body = if text.trim().is_empty() { "null" } else { &text };
            return serde_json::from_str(body)
                .map_err(|e| ApiError::Http { status, code: "BAD_RESPONSE".into(), message: format!("Onverwacht antwoord: {e}"), details: Value::Null });
        }
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let err = &v["error"];
        Err(ApiError::Http {
            status,
            code: err["code"].as_str().unwrap_or("HTTP_ERROR").to_string(),
            message: err["message"].as_str().unwrap_or("").to_string(),
            details: err["details"].clone(),
        })
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        Self::handle(self.agent.get(&self.url(path)).header("Authorization", &self.auth()).call())
    }

    fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T, ApiError> {
        Self::handle(self.agent.post(&self.url(path)).header("Authorization", &self.auth()).send_json(body))
    }

    /// POST /api/v1/claim — exchange a pairing code for a token.
    pub fn claim(&self, code: &str, device_name: &str) -> Result<ClaimResponse, ApiError> {
        let body = serde_json::json!({ "claim_code": code.trim().to_uppercase(), "device_name": device_name });
        Self::handle(self.agent.post(&self.url("/api/v1/claim")).send_json(&body))
    }

    pub fn me(&self) -> Result<MeResponse, ApiError> {
        self.get("/api/v1/me")
    }

    /// Newest Studio Recorder release in AudioShare (AudioShare 1.4.0+); None when unknown.
    pub fn latest_recorder(&self) -> Option<String> {
        let v: Value = self.get("/api/v1/me/recorder/latest").ok()?;
        v["version"].as_str().map(str::to_string)
    }

    pub fn programs(&self) -> Result<Vec<Program>, ApiError> {
        self.get::<Value>("/api/v1/me/programs").map(|v| parse_programs(&v))
    }

    pub fn upload_info(&self, program_id: i64) -> Result<UploadInfo, ApiError> {
        let v: Value = self.get(&format!("/api/v1/me/programs/{program_id}/upload-info"))?;
        Ok(UploadInfo {
            suggested_title: text(&v["suggested_title"]),
            suggested_description: text(&v["suggested_description"]),
            last_broadcast_date: v["last_broadcast_date"].as_str().map(str::to_string),
        })
    }

    /// Raw answers of /me and /me/programs, for troubleshooting (never contains the token).
    pub fn diagnose(&self) -> String {
        let mut out = format!(
            "StudioRecorder {} — AudioShare-diagnose {}\nAdres: {}\n",
            env!("CARGO_PKG_VERSION"),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            self.base
        );
        for path in ["/api/v1/me", "/api/v1/me/programs"] {
            out.push_str(&format!("\n=== GET {path}\n"));
            match self.agent.get(&self.url(path)).header("Authorization", &self.auth()).call() {
                Ok(mut r) => {
                    let status = r.status().as_u16();
                    let body = r.body_mut().read_to_string().unwrap_or_else(|e| format!("(niet leesbaar: {e})"));
                    let body: String = body.chars().take(20_000).collect();
                    out.push_str(&format!("HTTP {status}\n{body}\n"));
                    if path.ends_with("programs") {
                        let parsed = serde_json::from_str::<Value>(&body).map(|v| parse_programs(&v)).unwrap_or_default();
                        out.push_str(&format!("\n-> herkend: {} programma('s): {}\n", parsed.len(),
                            parsed.iter().map(|p| format!("{} (id {})", p.name, p.id)).collect::<Vec<_>>().join(", ")));
                    }
                }
                Err(e) => out.push_str(&format!("Geen antwoord: {e}\n")),
            }
        }
        out
    }

    pub fn create_episode(
        &self,
        program_id: i64,
        client_upload_id: &str,
        date: &str,
        title: &str,
        description: &str,
        operator: &str,
    ) -> Result<Draft, ApiError> {
        let mut body = serde_json::json!({
            "client_upload_id": client_upload_id, "broadcast_date": date,
            "title": title, "description": description, "parts": 1
        });
        if !operator.is_empty() {
            body["operator"] = operator.into();
        }
        self.post(&format!("/api/v1/me/programs/{program_id}/episodes"), &body)
    }

    pub fn replace(
        &self,
        episode_id: i64,
        client_upload_id: &str,
        title: &str,
        description: &str,
        confirm: bool,
        operator: &str,
    ) -> Result<Draft, ApiError> {
        let mut body = serde_json::json!({
            "client_upload_id": client_upload_id, "parts": 1,
            "title": title, "description": description, "confirm": confirm
        });
        if !operator.is_empty() {
            body["operator"] = operator.into();
        }
        self.post(&format!("/api/v1/me/episodes/{episode_id}/replace"), &body)
    }

    #[allow(dead_code)]
    pub fn get_draft(&self, draft_id: i64) -> Result<Value, ApiError> {
        self.get(&format!("/api/v1/me/drafts/{draft_id}"))
    }

    /// PUT one part. `sent` is updated with the number of bytes sent so far.
    pub fn put_part(&self, draft_id: i64, nr: u32, file: &Path, sha256_hex: &str, sent: Arc<AtomicU64>) -> Result<Value, ApiError> {
        let f = std::fs::File::open(file).map_err(|e| ApiError::Network(format!("bestand: {e}")))?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let ctype = match file.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("mp3") => "audio/mpeg",
            Some("flac") => "audio/flac",
            Some("ogg") => "audio/ogg",
            _ => "application/octet-stream",
        };
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // Header values must be ASCII: replace anything else in the name.
        let ascii_name: String = name.chars().map(|c| if c.is_ascii() && !c.is_ascii_control() { c } else { '_' }).collect();
        let mut reader = Counting { inner: f, sent };
        // Large uploads can take a while on a slow line: no overall timeout here.
        let up = agent(Duration::from_secs(6 * 3600));
        Self::handle(
            up.put(&self.url(&format!("/api/v1/me/drafts/{draft_id}/parts/{nr}")))
                .header("Authorization", &self.auth())
                .header("Content-Type", ctype)
                .header("Content-Length", &len.to_string())
                .header("X-Filename", &ascii_name)
                .header("X-Content-SHA256", sha256_hex)
                .send(ureq::SendBody::from_reader(&mut reader)),
        )
    }

    pub fn finalize(&self, draft_id: i64) -> Result<FinalizeResponse, ApiError> {
        self.post(&format!("/api/v1/me/drafts/{draft_id}/finalize"), &serde_json::json!({}))
    }
}

struct Counting {
    inner: std::fs::File,
    sent: Arc<AtomicU64>,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.sent.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// SHA-256 of a file as lowercase hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().replace(',', ".").parse().ok()))
}

fn list<T>(v: &Value, f: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    match v {
        Value::Array(a) => a.iter().filter_map(f).collect(),
        // "mp3,flac" as one string
        Value::String(s) => s.split([',', ' ', ';']).filter(|x| !x.is_empty()).filter_map(|x| f(&Value::String(x.to_string()))).collect(),
        _ => Vec::new(),
    }
}

fn parse_quality(q: &Value) -> Quality {
    Quality {
        formats: list(&q["formats"], |x| x.as_str().map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())),
        min_bitrate_kbps: int(&q["min_bitrate_kbps"]).map(|x| x as u32),
        samplerates: list(&q["samplerates"], |x| int(x).map(|i| i as u32)),
        min_duration_sec: int(&q["min_duration_sec"]).map(|x| x as u32),
        max_duration_sec: int(&q["max_duration_sec"]).map(|x| x as u32),
        max_file_mb: int(&q["max_file_mb"]).map(|x| x as u32),
        min_file_mb: int(&q["min_file_mb"]).map(|x| x as u32),
        lufs_min: num(&q["lufs_min"]),
        lufs_max: num(&q["lufs_max"]),
        true_peak_max: num(&q["true_peak_max"]),
    }
}

/// Read the programme list tolerantly: `{"programs": [...]}`, a bare array or
/// `{"data": [...]}`; ids as numbers or strings; missing or null fields allowed.
pub fn parse_programs(v: &Value) -> Vec<Program> {
    let arr = if v.is_array() {
        v.as_array()
    } else {
        v.get("programs").or_else(|| v.get("data")).and_then(Value::as_array)
    };
    let Some(arr) = arr else { return Vec::new() };
    arr.iter()
        .filter_map(|p| {
            let id = int(&p["id"]).or_else(|| int(&p["program_id"]))?;
            let mut name = text(&p["name"]);
            if name.is_empty() {
                name = text(&p["title"]);
            }
            if name.is_empty() {
                name = text(&p["slug"]);
            }
            Some(Program { id, slug: text(&p["slug"]), name, description: text(&p["description"]), quality: parse_quality(&p["quality"]) })
        })
        .collect()
}

/// Contents of a `.audioshare` pairing file (spec §1.3).
#[derive(Clone, Debug, Deserialize)]
pub struct LinkFile {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub user: String,
    pub claim_code: String,
    #[serde(default)]
    pub expires: String,
    /// "studio" when AudioShare made this file for a studio (Recorders → Studio's).
    #[serde(default, rename = "kind")]
    pub link_kind: String,
}

/// True when version `a` (x.y.z) is newer than `b`.
pub fn newer_version(a: &str, b: &str) -> bool {
    let p = |s: &str| s.trim().split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (p(a), p(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// `.audioshare` files next to the program (the zip from AudioShare → Recorders),
/// newest first, leaving out expired ones.
pub fn link_files_next_to_exe() -> Vec<std::path::PathBuf> {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("audioshare")))
        .filter(|p| match read_link_file(p) {
            Ok(l) => chrono::DateTime::parse_from_rfc3339(&l.expires).map_or(true, |t| t > chrono::Local::now()),
            Err(_) => false,
        })
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, p)| p).collect()
}

/// After a successful pairing the file is renamed, so it is not offered again.
pub fn retire_link_file(path: &Path) {
    let mut to = path.as_os_str().to_owned();
    to.push(".gebruikt");
    let _ = std::fs::rename(path, std::path::PathBuf::from(to));
}

pub fn read_link_file(path: &Path) -> Result<LinkFile, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("Kan koppelbestand niet lezen: {e}"))?;
    let l: LinkFile = serde_json::from_str(s.trim_start_matches('\u{feff}')).map_err(|_| "Dit is geen geldig koppelbestand.".to_string())?;
    if l.kind != "audioshare-maker-link" {
        return Err("Dit is geen AudioShare-koppelbestand.".into());
    }
    Ok(l)
}
