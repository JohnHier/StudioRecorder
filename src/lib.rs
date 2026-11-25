//! StudioRecorder and ArchiefRecorder: one code base, two programs
//! (see `src/main.rs` and `src/bin/archiefrecorder.rs`).

mod app;
pub mod audioshare;
pub mod auto;
mod auto_ui;
pub mod decode;
pub mod denoise;
pub mod edition;
pub mod engine;
pub mod export;
mod ipc;
pub mod level;
pub mod meta;
pub mod peaks;
pub mod player;
pub mod settings;
pub mod wav;

pub use edition::Edition;
use eframe::egui;

fn icon(edition: Edition) -> egui::IconData {
    let n = 64u32;
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    let c = (n as f32 - 1.0) / 2.0;
    // Studio: red record button. Archief: amber.
    let dot = match edition {
        Edition::Studio => [0xe5, 0x48, 0x4d],
        Edition::Archief => [0xf5, 0xb8, 0x3d],
    };
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let i = ((y * n + x) * 4) as usize;
            let (col, a): ([u8; 3], f32) = if d < 20.0 {
                (dot, (20.5 - d).clamp(0.0, 1.0))
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

pub fn run(edition: Edition) -> eframe::Result<()> {
    edition::set(edition);
    // Double-clicked a file while the program is already open: hand it over.
    if let Some(arg) = std::env::args().nth(1) {
        if ipc::handoff(&arg) {
            return Ok(());
        }
    }
    if edition == Edition::Studio {
        ipc::register_file_type();
    }
    let name = edition::app_name();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{name} v{}", edition::version()))
            .with_inner_size([1280.0, 880.0])
            .with_min_inner_size([980.0, 640.0])
            .with_icon(icon(edition)),
        ..Default::default()
    };
    eframe::run_native(name, options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
