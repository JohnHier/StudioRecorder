//! Per-computer settings, stored as JSON next to the user's app data.

use crate::export::ExportFormat;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// cpal device id of the input ("" = system default)
    pub input_id: String,
    /// cpal device id for playback ("" = system default)
    pub output_id: String,
    pub record_folder: String,
    /// Name used at the start of file names and in FLAC tags, e.g. "Studio 2".
    pub studio_name: String,
    pub export_format: ExportFormat,
    pub fade_ms: u32,
    /// Programme names to choose from on the recording screen.
    pub programs: Vec<String>,
    /// Per programme: title and description of the last episode.
    pub last_episode: HashMap<String, (String, String)>,
    /// Upload to AudioShare right after exporting.
    pub auto_upload: bool,
    /// Per AudioShare profile: the programme chosen last (makers with several programmes).
    pub last_program: HashMap<String, i64>,
    /// MP3 bitrate (StudioRecorder: always 320).
    pub mp3_kbps: u32,
    /// ArchiefRecorder: export in mono.
    pub mono: bool,
    /// ArchiefRecorder: volume leveler.
    pub level_on: bool,
    pub level: crate::level::LevelParams,
    /// ArchiefRecorder: peak limiter.
    pub limiter_on: bool,
    pub limiter: crate::level::LimiterParams,
    /// ArchiefRecorder: stop recording automatically after this many minutes (0 = off).
    pub auto_stop_min: u32,
    /// ... and/or after this many seconds of silence (tape hiss counts as silence).
    pub auto_stop_silence: bool,
    pub auto_stop_silence_s: u32,
    /// ArchiefRecorder: voice isolation (DeepFilterNet).
    pub isolate_on: bool,
    pub isolate: crate::denoise::Strength,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            input_id: String::new(),
            output_id: String::new(),
            record_folder: default_folder().to_string_lossy().into_owned(),
            studio_name: String::new(),
            export_format: if crate::edition::archive() { ExportFormat::Mp3 } else { ExportFormat::Flac },
            fade_ms: 10,
            programs: Vec::new(),
            last_episode: HashMap::new(),
            auto_upload: true,
            last_program: HashMap::new(),
            mp3_kbps: if crate::edition::archive() { 192 } else { 320 },
            mono: false,
            level_on: crate::edition::archive(),
            level: Default::default(),
            limiter_on: crate::edition::archive(),
            limiter: Default::default(),
            auto_stop_min: 0,
            auto_stop_silence: crate::edition::archive(),
            auto_stop_silence_s: 30,
            isolate_on: false,
            isolate: crate::denoise::Strength::Medium,
        }
    }
}

fn default_folder() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let music = home.join("Music");
    let base = if music.is_dir() { music } else { home };
    base.join(crate::edition::app_name())
}

/// %APPDATA%\StudioRecorder or %APPDATA%\ArchiefRecorder (or the XDG config folder elsewhere).
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(crate::edition::app_name())
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

/// Write a JSON file atomically (temp file + rename).
pub fn write_json<T: serde::Serialize>(path: &std::path::Path, value: &T) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(s) = serde_json::to_string_pretty(value) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let p = settings_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }

    /// Remember a programme and its latest episode details.
    pub fn remember(&mut self, meta: &crate::meta::Meta) {
        let p = meta.program.trim();
        if p.is_empty() {
            return;
        }
        if !self.programs.iter().any(|x| x == p) {
            self.programs.push(p.to_string());
            self.programs.sort_by_key(|a| a.to_lowercase());
        }
        if !meta.title.trim().is_empty() {
            self.last_episode.insert(p.to_string(), (meta.title.trim().to_string(), meta.description.trim().to_string()));
        }
    }
}
