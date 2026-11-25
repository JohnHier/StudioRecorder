#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    studiorecorder::run(studiorecorder::Edition::Studio)
}
