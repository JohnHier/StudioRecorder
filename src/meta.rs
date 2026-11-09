//! Programme information entered by the programme maker, stored next to
//! the recording as `<opname>.json` so it survives until the export.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub program: String,
    /// AudioShare programme id (when chosen from the AudioShare list).
    pub program_id: Option<i64>,
    /// Broadcast date as YYYY-MM-DD.
    pub date: String,
    pub title: String,
    pub description: String,
}

/// Characters Windows does not allow in file names are replaced.
pub fn safe_file_part(s: &str) -> String {
    let t: String = s
        .trim()
        .chars()
        .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    t.trim_matches(|c: char| c == '.' || c == ' ').to_string()
}

impl Meta {
    pub fn today() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    pub fn sidecar(wav: &Path) -> PathBuf {
        wav.with_extension("json")
    }

    pub fn load(wav: &Path) -> Option<Meta> {
        let s = std::fs::read_to_string(Self::sidecar(wav)).ok()?;
        serde_json::from_str(&s).ok()
    }

    pub fn save(&self, wav: &Path) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::sidecar(wav), s);
        }
    }

    /// Names of required fields that are still empty.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.program.trim().is_empty() {
            v.push("programma");
        }
        if self.date.trim().is_empty() {
            v.push("uitzenddag");
        }
        if self.title.trim().is_empty() {
            v.push("titel");
        }
        v
    }

    /// Default export name: "Programma 2026-10-12 - Titel".
    pub fn export_name(&self) -> Option<String> {
        if self.program.trim().is_empty() {
            return None;
        }
        let mut n = format!("{} {}", self.program.trim(), self.date.trim());
        if !self.title.trim().is_empty() {
            n.push_str(" - ");
            n.push_str(self.title.trim());
        }
        Some(safe_file_part(&n))
    }

    pub fn tags(&self, studio: &str) -> Vec<(String, String)> {
        let mut t = Vec::new();
        let mut add = |k: &str, v: &str| {
            if !v.trim().is_empty() {
                t.push((k.to_string(), v.trim().to_string()));
            }
        };
        add("TITLE", &self.title);
        add("ALBUM", &self.program);
        add("ARTIST", &self.program);
        add("DATE", &self.date);
        add("DESCRIPTION", &self.description);
        add("COMMENT", if self.description.trim().is_empty() { "Opgenomen met StudioRecorder" } else { &self.description });
        if studio.trim() != "Opname" {
            add("ORGANIZATION", studio);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_name_is_windows_safe() {
        let m = Meta { program: "Ochtend: Show".into(), date: "2026-10-12".into(), title: "Deel 1/2?".into(), ..Default::default() };
        assert_eq!(m.export_name().unwrap(), "Ochtend_ Show 2026-10-12 - Deel 1_2_");
        assert!(m.missing().is_empty());
        assert_eq!(Meta::default().missing(), vec!["programma", "uitzenddag", "titel"]);
    }
}
