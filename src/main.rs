#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod audioshare;
mod decode;
mod engine;
mod export;
mod ipc;
mod meta;
mod peaks;
mod player;
mod settings;
mod wav;

use eframe::egui;

fn icon() -> egui::IconData {
    let n = 64u32;
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    let c = (n as f32 - 1.0) / 2.0;
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let i = ((y * n + x) * 4) as usize;
            let (col, a): ([u8; 3], f32) = if d < 20.0 {
                ([0xe5, 0x48, 0x4d], (20.5 - d).clamp(0.0, 1.0))
            } else if d < 25.0 {
                ([0x1d, 0x20, 0x27], 1.0)
            } else if d < 31.0 {
                ([0xe8, 0xea, 0xee], (31.5 - d).clamp(0.0, 1.0))
            } else {
                ([0, 0, 0], 0.0)
            };
            rgba[i..i + 3].copy_from_slice(&col);
            rgba[i + 3] = (a * 255.0) as u8;
        }
    }
    egui::IconData { rgba, width: n, height: n }
}

fn main() -> eframe::Result<()> {
    // Double-clicked a file while StudioRecorder is already open: hand it over.
    if let Some(arg) = std::env::args().nth(1) {
        if ipc::handoff(&arg) {
            return Ok(());
        }
    }
    ipc::register_file_type();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(concat!("StudioRecorder v", env!("CARGO_PKG_VERSION")))
            .with_inner_size([1280.0, 880.0])
            .with_min_inner_size([980.0, 640.0])
            .with_icon(icon()),
        ..Default::default()
    };
    eframe::run_native("StudioRecorder", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
