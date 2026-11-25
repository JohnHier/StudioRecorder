//! Which program this is: StudioRecorder (radio studio, AudioShare) or
//! ArchiefRecorder (digitising tapes: leveler, no AudioShare). Same code base,
//! two programs with their own settings folder, name and icon.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edition {
    Studio,
    Archief,
}

static EDITION: OnceLock<Edition> = OnceLock::new();

pub fn set(e: Edition) {
    let _ = EDITION.set(e);
}

pub fn get() -> Edition {
    *EDITION.get().unwrap_or(&Edition::Studio)
}

pub fn archive() -> bool {
    get() == Edition::Archief
}

/// Version shown to the user. ArchiefRecorder has its own numbering.
pub fn version() -> &'static str {
    match get() {
        Edition::Studio => env!("CARGO_PKG_VERSION"),
        Edition::Archief => ARCHIEF_VERSION,
    }
}

pub const ARCHIEF_VERSION: &str = "1.2.0";

/// Program name, also used for the settings folder (%APPDATA%\<name>).
pub fn app_name() -> &'static str {
    match get() {
        Edition::Studio => "StudioRecorder",
        Edition::Archief => "ArchiefRecorder",
    }
}
