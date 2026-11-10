//! The user interface: a recording screen and a trim/export screen.

use crate::engine::{self, Engine, InputChoice};
use crate::export::{self, ExportFormat, ExportJob};
use crate::meta::{Meta, safe_file_part};

#[path = "as_ui.rs"]
mod as_ui;
use crate::peaks::Peaks;
use crate::player::{self, Player};
use crate::settings::Settings;
use crate::wav::{WavFormat, WavReader};
use crossbeam_channel::Receiver;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Pos2, Rect, Sense, Stroke, StrokeKind,
    pos2, vec2,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

const BG: Color32 = Color32::from_rgb(0x15, 0x17, 0x1c);
const PANEL: Color32 = Color32::from_rgb(0x1d, 0x20, 0x27);
const PANEL2: Color32 = Color32::from_rgb(0x27, 0x2b, 0x34);
const LINE: Color32 = Color32::from_rgb(0x34, 0x39, 0x44);
const TEXT: Color32 = Color32::from_rgb(0xe8, 0xea, 0xee);
const DIM: Color32 = Color32::from_rgb(0x8e, 0x95, 0xa2);
const RED: Color32 = Color32::from_rgb(0xe5, 0x48, 0x4d);
const RED_DARK: Color32 = Color32::from_rgb(0x5c, 0x1d, 0x21);
const GREEN: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x8c);
const AMBER: Color32 = Color32::from_rgb(0xf5, 0xb8, 0x3d);
const WAVE: Color32 = Color32::from_rgb(0x5f, 0xb3, 0xff);
const WAVE_DIM: Color32 = Color32::from_rgb(0x3a, 0x45, 0x55);
const WAVE_CUT: Color32 = Color32::from_rgb(0x6e, 0x3c, 0x42);
const IN_COL: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x8c);
const OUT_COL: Color32 = Color32::from_rgb(0xff, 0x8a, 0x4c);
const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
/// Placeholder text in empty fields: clearly not real input.
const LOGO_W: usize = 414;
const LOGO_H: usize = 72;

/// The JohnWorks logo, small and slightly dimmed.
fn logo(ui: &mut egui::Ui, tex: &egui::TextureHandle, height: f32) -> egui::Response {
    let size = vec2(height * LOGO_W as f32 / LOGO_H as f32, height);
    ui.add(egui::Image::new((tex.id(), size)).tint(Color32::from_white_alpha(170)))
}

const HINT: Color32 = Color32::from_rgb(0x5c, 0x62, 0x6e);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fmt_time(frames: u64, rate: u32, millis: bool) -> String {
    let rate = rate.max(1) as u64;
    let total_ms = frames * 1000 / rate;
    let h = total_ms / 3_600_000;
    let m = (total_ms / 60_000) % 60;
    let s = (total_ms / 1000) % 60;
    let ms = total_ms % 1000;
    if millis {
        format!("{h:02}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}

fn db(v: f32) -> f32 {
    if v <= 1e-6 { -120.0 } else { 20.0 * v.log10() }
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

#[cfg(windows)]
fn free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            return None;
        }
    }
    let w: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free: u64 = 0;
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    if ok != 0 { Some(free) } else { None }
}

#[cfg(not(windows))]
fn free_space(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            return None;
        }
    }
    let out = std::process::Command::new("df").arg("-k").arg("--output=avail").arg(&p).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().nth(1)?.trim().parse::<u64>().ok().map(|k| k * 1024)
}

fn show_in_folder(path: &Path) {
    #[cfg(windows)]
    {
        // explorer wants /select,"path" unquoted as a whole; Rust's normal
        // argument quoting breaks it for paths with spaces (it then opens Documents).
        use std::os::windows::process::CommandExt;
        let p = path.to_string_lossy().replace('/', "\\");
        let p = p.strip_prefix(r"\\?\").unwrap_or(&p).to_string();
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{p}\"")).spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(d) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(d).spawn();
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Stop,
    Play,
    Pause,
    ToIn,
    ToOut,
    Folder,
    Gear,
    Plus,
    Minus,
    Fit,
    Cut,
}

fn paint_icon(p: &egui::Painter, icon: Icon, c: Pos2, s: f32, col: Color32) {
    let h = s * 0.5;
    match icon {
        Icon::Stop => {
            p.rect_filled(Rect::from_center_size(c, vec2(s * 0.8, s * 0.8)), 2.0, col);
        }
        Icon::Play => {
            p.add(egui::Shape::convex_polygon(
                vec![pos2(c.x - h * 0.7, c.y - h), pos2(c.x + h, c.y), pos2(c.x - h * 0.7, c.y + h)],
                col,
                Stroke::NONE,
            ));
        }
        Icon::Pause => {
            p.rect_filled(Rect::from_min_max(pos2(c.x - h * 0.8, c.y - h), pos2(c.x - h * 0.2, c.y + h)), 1.0, col);
            p.rect_filled(Rect::from_min_max(pos2(c.x + h * 0.2, c.y - h), pos2(c.x + h * 0.8, c.y + h)), 1.0, col);
        }
        Icon::ToIn => {
            p.rect_filled(Rect::from_min_max(pos2(c.x - h, c.y - h), pos2(c.x - h * 0.65, c.y + h)), 0.0, col);
            p.add(egui::Shape::convex_polygon(
                vec![pos2(c.x - h * 0.45, c.y - h), pos2(c.x + h, c.y), pos2(c.x - h * 0.45, c.y + h)],
                col,
                Stroke::NONE,
            ));
        }
        Icon::ToOut => {
            p.add(egui::Shape::convex_polygon(
                vec![pos2(c.x - h, c.y - h), pos2(c.x + h * 0.45, c.y), pos2(c.x - h, c.y + h)],
                col,
                Stroke::NONE,
            ));
            p.rect_filled(Rect::from_min_max(pos2(c.x + h * 0.65, c.y - h), pos2(c.x + h, c.y + h)), 0.0, col);
        }
        Icon::Folder => {
            let r = Rect::from_center_size(c + vec2(0.0, h * 0.1), vec2(s, s * 0.7));
            p.rect_filled(Rect::from_min_size(r.min - vec2(0.0, h * 0.25), vec2(s * 0.45, h * 0.4)), 1.5, col);
            p.rect_filled(r, 2.0, col);
        }
        Icon::Gear => {
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([c + d * h * 0.55, c + d * h], Stroke::new(s * 0.18, col));
            }
            p.circle_stroke(c, h * 0.55, Stroke::new(s * 0.17, col));
        }
        Icon::Plus => {
            p.line_segment([pos2(c.x - h, c.y), pos2(c.x + h, c.y)], Stroke::new(2.0, col));
            p.line_segment([pos2(c.x, c.y - h), pos2(c.x, c.y + h)], Stroke::new(2.0, col));
        }
        Icon::Minus => {
            p.line_segment([pos2(c.x - h, c.y), pos2(c.x + h, c.y)], Stroke::new(2.0, col));
        }
        Icon::Cut => {
            // scissors: two rings and two crossing blades
            let st = Stroke::new(1.6, col);
            p.circle_stroke(pos2(c.x - h * 0.5, c.y + h * 0.6), h * 0.32, st);
            p.circle_stroke(pos2(c.x + h * 0.5, c.y + h * 0.6), h * 0.32, st);
            p.line_segment([pos2(c.x - h * 0.3, c.y + h * 0.35), pos2(c.x + h * 0.55, c.y - h)], st);
            p.line_segment([pos2(c.x + h * 0.3, c.y + h * 0.35), pos2(c.x - h * 0.55, c.y - h)], st);
        }
        Icon::Fit => {
            p.line_segment([pos2(c.x - h, c.y), pos2(c.x + h, c.y)], Stroke::new(1.5, col));
            p.line_segment([pos2(c.x - h, c.y - h), pos2(c.x - h, c.y + h)], Stroke::new(1.5, col));
            p.line_segment([pos2(c.x + h, c.y - h), pos2(c.x + h, c.y + h)], Stroke::new(1.5, col));
        }
    }
}

/// A flat button with a painted icon and a label.
fn ibutton(ui: &mut egui::Ui, icon: Option<Icon>, label: &str, enabled: bool, accent: Option<Color32>) -> egui::Response {
    let font = FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, TEXT);
    let icon_w = if icon.is_some() { 18.0 } else { 0.0 };
    let gap = if icon.is_some() && !label.is_empty() { 7.0 } else { 0.0 };
    let size = vec2(icon_w + gap + galley.size().x + 22.0, 32.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let base = accent.unwrap_or(PANEL2);
        let fill = if !enabled {
            PANEL
        } else if resp.is_pointer_button_down_on() {
            base.gamma_multiply(0.8)
        } else if hovered {
            base.linear_multiply(1.25)
        } else {
            base
        };
        ui.painter().rect_filled(rect, 6.0, fill);
        if accent.is_none() {
            ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0, LINE), StrokeKind::Inside);
        }
        let col = if enabled { TEXT } else { DIM.gamma_multiply(0.6) };
        let mut x = rect.left() + 11.0;
        if let Some(i) = icon {
            paint_icon(ui.painter(), i, pos2(x + 9.0, rect.center().y), 13.0, col);
            x += icon_w + gap;
        }
        ui.painter().galley_with_override_text_color(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            col,
        );
    }
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

// ---------------------------------------------------------------------------
// Programme information form
// ---------------------------------------------------------------------------

const PURPLE: Color32 = Color32::from_rgb(0xa7, 0x8b, 0xfa);

#[derive(Default)]
struct FormState {
    title_hint: bool,
    desc_hint: bool,
    /// Text of a programme being added (Some = add mode).
    adding: Option<String>,
    focus_add: bool,
    /// AudioShare programme whose upload-info should be fetched.
    want_info: Option<i64>,
    /// The user asked to sign in (shared studio).
    want_login: bool,
    /// Fetch the programme list again.
    want_refresh: bool,
    /// Title/description came from AudioShare rather than this PC.
    remote_hint: bool,
}


const WEEKDAYS_NL: [&str; 7] = ["ma", "di", "wo", "do", "vr", "za", "zo"];
const MONTHS_NL: [&str; 12] =
    ["januari", "februari", "maart", "april", "mei", "juni", "juli", "augustus", "september", "oktober", "november", "december"];

/// A date button ("ma 12-10-2026") with a Dutch calendar popup. `date` is YYYY-MM-DD.
fn date_picker(ui: &mut egui::Ui, date: &mut String, id: &str, width: f32) -> bool {
    use chrono::{Datelike, Duration as CDur, NaiveDate};
    let today = chrono::Local::now().date_naive();
    let mut d = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap_or(today);
    let label = format!("{}  {}", WEEKDAYS_NL[d.weekday().num_days_from_monday() as usize], d.format("%d-%m-%Y"));
    let resp = ui.add_sized([width, 30.0], egui::Button::new(egui::RichText::new(label).size(14.5)));
    let month_id = ui.id().with(id).with("month");
    if resp.clicked() {
        ui.data_mut(|m| m.insert_temp(month_id, d.with_day(1).unwrap()));
    }
    let mut changed = false;
    egui::Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let mut month: NaiveDate = ui.data(|m| m.get_temp(month_id)).unwrap_or(d.with_day(1).unwrap());
            ui.set_min_width(250.0);
            ui.horizontal(|ui| {
                if ui.button(" < ").clicked() {
                    month = (month - CDur::days(1)).with_day(1).unwrap();
                }
                let title = format!("{} {}", MONTHS_NL[month.month0() as usize], month.year());
                ui.add_sized([150.0, 24.0], egui::Label::new(egui::RichText::new(title).strong()));
                if ui.button(" > ").clicked() {
                    month = (month + CDur::days(32)).with_day(1).unwrap();
                }
            });
            egui::Grid::new(ui.id().with("days")).spacing(vec2(2.0, 2.0)).show(ui, |ui| {
                for w in WEEKDAYS_NL {
                    ui.add_sized([32.0, 20.0], egui::Label::new(egui::RichText::new(w).color(DIM).size(12.0)));
                }
                ui.end_row();
                let offset = month.weekday().num_days_from_monday();
                for _ in 0..offset {
                    ui.label("");
                }
                let mut day = month;
                let mut col = offset;
                while day.month() == month.month() {
                    let selected = day == d;
                    let mut text = egui::RichText::new(day.day().to_string());
                    if day == today {
                        text = text.strong().color(PURPLE);
                    }
                    if ui.add_sized([32.0, 26.0], egui::Button::selectable(selected, text)).clicked() {
                        d = day;
                        changed = true;
                        ui.close();
                    }
                    col += 1;
                    if col == 7 {
                        col = 0;
                        ui.end_row();
                    }
                    day += CDur::days(1);
                }
            });
            ui.add_space(4.0);
            if ui.button("Vandaag").clicked() {
                d = today;
                changed = true;
                ui.close();
            }
            ui.data_mut(|m| m.insert_temp(month_id, month));
        });
    if changed {
        *date = d.format("%Y-%m-%d").to_string();
    }
    changed
}

/// Round pause/resume button next to the record button.
fn pause_button(ui: &mut egui::Ui, size: f32, enabled: bool, paused: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let hovered = resp.hovered() && enabled;
    let p = ui.painter();
    let c = rect.center();
    let ring = if paused { AMBER } else { LINE };
    p.circle_filled(c, size * 0.5, if hovered { PANEL2.linear_multiply(1.3) } else { PANEL2 });
    p.circle_stroke(c, size * 0.5 - 1.0, Stroke::new(2.0, if enabled { ring } else { LINE.gamma_multiply(0.5) }));
    let col = if !enabled { DIM.gamma_multiply(0.4) } else if paused { AMBER } else { TEXT };
    paint_icon(p, if paused { Icon::Play } else { Icon::Pause }, c, size * 0.36, col);
    let tip = if !enabled { "Pauzeren kan tijdens een opname" } else if paused { "Opname hervatten" } else { "Opname pauzeren" };
    enabled && resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked()
}

/// Where the programme list comes from.
enum ProgramSource<'a> {
    /// No AudioShare link: names kept on this PC.
    Local,
    /// Programmes assigned to the signed-in maker in AudioShare.
    Remote(&'a [crate::audioshare::api::Program]),
    /// AudioShare is set up, but nobody is signed in (shared studio).
    Locked,
}

fn program_picker(
    ui: &mut egui::Ui,
    meta: &mut Meta,
    settings: &mut Settings,
    st: &mut FormState,
    id: &str,
    width: f32,
    src: &ProgramSource,
) -> bool {
    match src {
        ProgramSource::Local => program_picker_local(ui, meta, settings, st, id, width),
        ProgramSource::Locked => {
            ui.horizontal(|ui| {
                let label = if meta.program.is_empty() {
                    "Log in om te kiezen".to_string()
                } else {
                    format!("{}  (inloggen om te wijzigen)", meta.program)
                };
                ui.add_sized([(width - 110.0).max(100.0), 30.0], egui::Label::new(egui::RichText::new(label).color(DIM)).truncate());
                if ui.button("Inloggen").clicked() {
                    st.want_login = true;
                }
            });
            false
        }
        ProgramSource::Remote(list) => {
            let shown = if meta.program.is_empty() {
                egui::RichText::new("Kies een programma…").color(DIM).size(14.5)
            } else {
                egui::RichText::new(&meta.program).size(14.5)
            };
            let mut pick: Option<(i64, String)> = None;
            egui::ComboBox::from_id_salt(id).width(width).height(400.0).selected_text(shown).show_ui(ui, |ui| {
                if list.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "AudioShare geeft geen programma's voor je terug.\nLaat de beheerder in AudioShare een programma aan je toewijzen.",
                        )
                        .color(AMBER),
                    );
                }
                for p in list.iter() {
                    if ui.selectable_label(meta.program_id == Some(p.id), &p.name).clicked() {
                        pick = Some((p.id, p.name.clone()));
                    }
                }
                ui.separator();
                if ui.selectable_label(false, egui::RichText::new("Lijst vernieuwen uit AudioShare").color(PURPLE)).clicked() {
                    st.want_refresh = true;
                }
            });
            match pick {
                Some((pid, name)) if meta.program_id != Some(pid) || meta.program != name => {
                    meta.program = name;
                    meta.program_id = Some(pid);
                    st.want_info = Some(pid);
                    // Text that was only suggested for the previous programme goes.
                    if st.title_hint {
                        meta.title.clear();
                        st.title_hint = false;
                    }
                    if st.desc_hint {
                        meta.description.clear();
                        st.desc_hint = false;
                    }
                    true
                }
                _ => false,
            }
        }
    }
}

/// Fill in title/description suggested by AudioShare (only where the user
/// has not typed anything themselves).
fn apply_upload_info(meta: &mut Meta, st: &mut FormState, info: &crate::audioshare::api::UploadInfo) -> bool {
    let mut changed = false;
    if (meta.title.trim().is_empty() || st.title_hint) && !info.suggested_title.trim().is_empty() {
        meta.title = info.suggested_title.clone();
        st.title_hint = true;
        changed = true;
    }
    if (meta.description.trim().is_empty() || st.desc_hint) && !info.suggested_description.trim().is_empty() {
        meta.description = info.suggested_description.clone();
        st.desc_hint = true;
        changed = true;
    }
    st.remote_hint = true;
    changed
}

/// Programme dropdown with "nieuw programma"; fills in the last episode's
/// title and description when a programme is chosen.
fn program_picker_local(ui: &mut egui::Ui, meta: &mut Meta, settings: &mut Settings, st: &mut FormState, id: &str, width: f32) -> bool {
    let mut changed = false;
    if let Some(name) = st.adding.as_mut() {
        let mut done: Option<bool> = None;
        ui.horizontal(|ui| {
            let te = ui.add(
                egui::TextEdit::singleline(name)
                    .hint_text(egui::RichText::new("Naam van het nieuwe programma").color(HINT))
                    .desired_width((width - 150.0).max(120.0))
                    .margin(vec2(8.0, 6.0)),
            );
            if st.focus_add {
                te.request_focus();
                st.focus_add = false;
            }
            if te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                done = Some(true);
            }
            if ui.button("Toevoegen").clicked() {
                done = Some(true);
            }
            if ui.button("×").on_hover_text("Annuleren").clicked() {
                done = Some(false);
            }
        });
        match done {
            Some(true) => {
                let n = name.trim().to_string();
                st.adding = None;
                if !n.is_empty() {
                    if !settings.programs.iter().any(|x| *x == n) {
                        settings.programs.push(n.clone());
                        settings.programs.sort_by_key(|a| a.to_lowercase());
                        settings.save();
                    }
                    meta.program = n;
                    meta.program_id = None;
                    st.title_hint = false;
                    st.desc_hint = false;
                    changed = true;
                }
            }
            Some(false) => st.adding = None,
            None => {}
        }
        return changed;
    }
    let shown = if meta.program.is_empty() {
        egui::RichText::new("Kies een programma…").color(DIM).size(14.5)
    } else {
        egui::RichText::new(&meta.program).size(14.5)
    };
    let mut pick: Option<String> = None;
    let mut add = false;
    let cb = egui::ComboBox::from_id_salt(id).width(width).height(400.0).selected_text(shown).show_ui(ui, |ui| {
        for p in &settings.programs {
            if ui.selectable_label(*p == meta.program, p).clicked() {
                pick = Some(p.clone());
            }
        }
        if !settings.programs.is_empty() {
            ui.separator();
        }
        if ui.selectable_label(false, egui::RichText::new("+ Nieuw programma…").color(PURPLE)).clicked() {
            add = true;
        }
    });
    let _ = cb;
    if add {
        st.adding = Some(String::new());
        st.focus_add = true;
    }
    if let Some(p) = pick {
        if p != meta.program || meta.program_id.is_some() {
            meta.program = p.clone();
            meta.program_id = None;
            st.remote_hint = false;
            changed = true;
            match settings.last_episode.get(&p) {
                Some((t, d)) => {
                    if meta.title.trim().is_empty() || st.title_hint {
                        meta.title = t.clone();
                        st.title_hint = !t.is_empty();
                    }
                    if meta.description.trim().is_empty() || st.desc_hint {
                        meta.description = d.clone();
                        st.desc_hint = !d.is_empty();
                    }
                }
                None => {
                    // Clear text that was only filled in from another programme.
                    if st.title_hint {
                        meta.title.clear();
                        st.title_hint = false;
                    }
                    if st.desc_hint {
                        meta.description.clear();
                        st.desc_hint = false;
                    }
                }
            }
        }
    }
    changed
}

fn hint_line(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(12.5).color(PURPLE));
}


// ---------------------------------------------------------------------------
// Waveform drawing
// ---------------------------------------------------------------------------

enum WaveSource<'a> {
    Peaks(&'a Peaks),
    /// Raw interleaved samples starting at frame `.1`
    Samples(&'a [f32], u64, usize),
}

/// Draw channels as stacked lanes for frames [start, start + len).
fn draw_wave(
    p: &egui::Painter,
    rect: Rect,
    src: &WaveSource,
    channels: usize,
    start: f64,
    len: f64,
    color_for: &dyn Fn(f64) -> Color32,
) {
    let w = rect.width().max(1.0) as usize;
    let lanes = channels.clamp(1, 2);
    let lane_h = rect.height() / lanes as f32;
    let fpp = len / w as f64; // frames per pixel
    for lane in 0..lanes {
        let top = rect.top() + lane as f32 * lane_h;
        let mid = top + lane_h / 2.0;
        let amp = lane_h / 2.0 - 2.0;
        p.line_segment([pos2(rect.left(), mid), pos2(rect.right(), mid)], Stroke::new(1.0, LINE));
        if lane > 0 {
            p.line_segment([pos2(rect.left(), top), pos2(rect.right(), top)], Stroke::new(1.0, BG));
        }
        let mut prev: Option<Pos2> = None;
        for x in 0..w {
            let a = start + x as f64 * fpp;
            let b = a + fpp;
            if b <= 0.0 {
                continue;
            }
            let mm = match src {
                WaveSource::Peaks(pk) => pk.range(lane, a.max(0.0) as u64, (b.max(a + 1.0)) as u64),
                WaveSource::Samples(s, s0, ch) => {
                    let ch = *ch;
                    let fa = (a.floor() as i64 - *s0 as i64).max(0) as usize;
                    let fb = ((b.ceil() as i64 - *s0 as i64).max(0) as usize).max(fa + 1);
                    let frames = s.len() / ch;
                    if fa >= frames {
                        None
                    } else {
                        let mut mm = (f32::MAX, f32::MIN);
                        for f in fa..fb.min(frames) {
                            let v = s[f * ch + lane.min(ch - 1)];
                            mm.0 = mm.0.min(v);
                            mm.1 = mm.1.max(v);
                        }
                        Some(mm)
                    }
                }
            };
            let Some((mn, mx)) = mm else {
                prev = None;
                continue;
            };
            let col = color_for(a);
            let px = rect.left() + x as f32 + 0.5;
            if fpp < 1.5 {
                // Zoomed in to individual samples: draw a connected line.
                let y = mid - ((mn + mx) * 0.5).clamp(-1.0, 1.0) * amp;
                let pt = pos2(px, y);
                if let Some(pp) = prev {
                    p.line_segment([pp, pt], Stroke::new(1.5, col));
                }
                prev = Some(pt);
            } else {
                let y1 = mid - mx.clamp(-1.0, 1.0) * amp;
                let y2 = mid - mn.clamp(-1.0, 1.0) * amp;
                p.line_segment([pos2(px, y1), pos2(px, y2.max(y1 + 1.0))], Stroke::new(1.0, col));
            }
        }
    }
}

/// Time ruler with labels chosen to fit the zoom level.
fn draw_ruler(p: &egui::Painter, rect: Rect, start: f64, len: f64, rate: u32) {
    p.rect_filled(rect, 0.0, PANEL);
    let rate_f = rate as f64;
    let secs_per_px = len / rate_f / rect.width() as f64;
    let steps = [
        0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
    ];
    let step = steps.iter().copied().find(|s| s / secs_per_px >= 90.0).unwrap_or(7200.0);
    let t0 = start / rate_f;
    let t1 = (start + len) / rate_f;
    let mut t = (t0 / step).floor() * step;
    let font = FontId::monospace(11.0);
    while t <= t1 {
        if t >= 0.0 {
            let x = rect.left() + ((t * rate_f - start) / len) as f32 * rect.width();
            p.line_segment([pos2(x, rect.bottom() - 7.0), pos2(x, rect.bottom())], Stroke::new(1.0, DIM));
            let label = if step < 1.0 {
                let ms = (t * 1000.0).round() as u64;
                format!("{}:{:02}.{:03}", ms / 60000, (ms / 1000) % 60, ms % 1000)
            } else {
                let s = t.round() as u64;
                if s >= 3600 {
                    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
                } else {
                    format!("{}:{:02}", s / 60, s % 60)
                }
            };
            p.text(pos2(x + 3.0, rect.top() + 2.0), Align2::LEFT_TOP, label, font.clone(), DIM);
        }
        t += step;
    }
}

// ---------------------------------------------------------------------------
// Level meter
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Meter {
    level: [f32; 2], // dB shown
    hold: [f32; 2],
    hold_at: [Option<Instant>; 2],
    clip: [bool; 2],
    last: Option<Instant>,
}

const METER_MIN: f32 = -60.0;

impl Meter {
    fn update(&mut self, peak: [f32; 2]) {
        let now = Instant::now();
        let dt = self.last.map(|l| now.duration_since(l).as_secs_f32()).unwrap_or(0.0).min(0.5);
        self.last = Some(now);
        for c in 0..2 {
            let d = db(peak[c]).max(METER_MIN);
            // instant attack, ~13 dB/s release (PPM-like)
            self.level[c] = if d >= self.level[c] { d } else { (self.level[c] - 13.0 * dt).max(d).max(METER_MIN) };
            let expired = self.hold_at[c].is_none_or(|t| now.duration_since(t) > Duration::from_millis(2000));
            if d >= self.hold[c] || expired {
                self.hold[c] = d;
                self.hold_at[c] = Some(now);
            }
            if peak[c] >= 0.999 {
                self.clip[c] = true;
            }
        }
    }

    fn reset(&mut self) {
        *self = Meter { level: [METER_MIN; 2], hold: [METER_MIN; 2], ..Default::default() };
    }

    fn ui(&mut self, ui: &mut egui::Ui, height: f32, channels: usize) {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::click());
        let p = ui.painter_at(rect);
        let clip_h = 20.0;
        let bar_w = 22.0;
        let gap = 6.0;
        let scale_w = 30.0;
        let total_w = scale_w + bar_w * 2.0 + gap;
        let x0 = rect.center().x - total_w / 2.0;
        let top = rect.top() + clip_h + 6.0;
        let bottom = rect.bottom() - 40.0;
        let y_of = |d: f32| {
            // slightly expanded top end: -20..0 gets half the height
            let t = if d >= -20.0 { 0.5 + 0.5 * (d + 20.0) / 20.0 } else { 0.5 * (d - METER_MIN) / (-20.0 - METER_MIN) };
            bottom - t.clamp(0.0, 1.0) * (bottom - top)
        };
        let font = FontId::monospace(10.0);
        for d in [0.0, -3.0, -6.0, -9.0, -12.0, -18.0, -24.0, -30.0, -40.0, -50.0, -60.0] {
            let y = y_of(d);
            p.text(pos2(x0 + scale_w - 6.0, y), Align2::RIGHT_CENTER, format!("{}", d as i32), font.clone(), DIM);
            p.line_segment(
                [pos2(x0 + scale_w - 3.0, y), pos2(x0 + total_w, y)],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 14)),
            );
        }
        for c in 0..2 {
            let ch = if channels == 1 { 0 } else { c };
            let bx = x0 + scale_w + c as f32 * (bar_w + gap);
            let bar = Rect::from_min_max(pos2(bx, top), pos2(bx + bar_w, bottom));
            p.rect_filled(bar, 2.0, Color32::from_rgb(0x10, 0x12, 0x16));
            // segmented fill
            let level = self.level[ch];
            let seg = 3.0;
            let mut y = bottom;
            while y > top {
                let y2 = (y - seg + 1.0).max(top);
                let t = (bottom - y) / (bottom - top);
                let d_here = if t <= 0.5 {
                    METER_MIN + t / 0.5 * (-20.0 - METER_MIN)
                } else {
                    -20.0 + (t - 0.5) / 0.5 * 20.0
                };
                let col = if d_here > -6.0 {
                    RED
                } else if d_here > -18.0 {
                    AMBER
                } else {
                    GREEN
                };
                let lit = d_here <= level && level > METER_MIN + 0.5;
                let colr = if lit { col } else { col.gamma_multiply(0.12) };
                p.rect_filled(Rect::from_min_max(pos2(bx + 1.0, y2), pos2(bx + bar_w - 1.0, y)), 0.0, colr);
                y -= seg;
            }
            // peak hold
            if self.hold[ch] > METER_MIN + 0.5 {
                let hy = y_of(self.hold[ch]);
                p.line_segment([pos2(bx + 1.0, hy), pos2(bx + bar_w - 1.0, hy)], Stroke::new(2.0, TEXT));
            }
            // clip LED
            let led = Rect::from_min_size(pos2(bx, rect.top()), vec2(bar_w, clip_h - 4.0));
            p.rect_filled(led, 3.0, if self.clip[ch] { RED } else { RED_DARK.gamma_multiply(0.6) });
            p.text(led.center(), Align2::CENTER_CENTER, "CLIP", FontId::proportional(8.5), if self.clip[ch] { TEXT } else { DIM.gamma_multiply(0.5) });
            p.text(
                pos2(bar.center().x, bottom + 4.0),
                Align2::CENTER_TOP,
                if channels == 1 { "M" } else if c == 0 { "L" } else { "R" },
                FontId::proportional(12.0),
                DIM,
            );
        }
        let hold = self.hold[0].max(self.hold[1]);
        let txt = if hold <= METER_MIN + 0.5 { "-∞".to_string() } else { format!("{hold:.1}") };
        p.text(pos2(rect.center().x, rect.bottom() - 2.0), Align2::CENTER_BOTTOM, format!("piek {txt} dBFS"), FontId::monospace(10.5), TEXT);
        if resp.clicked() {
            self.clip = [false; 2];
        }
        resp.on_hover_text("Klik om de CLIP-lampjes te resetten");
    }
}

// ---------------------------------------------------------------------------
// Editor state
// ---------------------------------------------------------------------------

/// Crossfade at every cut in the exported file.
const CROSSFADE_MS: u32 = 10;

/// The last `n` frames of a list of ranges.
fn segs_tail(segs: &[(u64, u64)], n: u64) -> Vec<(u64, u64)> {
    let mut left = n;
    let mut v = Vec::new();
    for &(a, b) in segs.iter().rev() {
        if left == 0 {
            break;
        }
        let take = (b - a).min(left);
        v.push((b - take, b));
        left -= take;
    }
    v.reverse();
    v
}

/// The first `n` frames of a list of ranges.
fn segs_head(segs: &[(u64, u64)], n: u64) -> Vec<(u64, u64)> {
    let mut left = n;
    let mut v = Vec::new();
    for &(a, b) in segs {
        if left == 0 {
            break;
        }
        let take = (b - a).min(left);
        v.push((a, a + take));
        left -= take;
    }
    v
}

fn fmt_secs(frames: u64, rate: u32) -> String {
    let s = frames as f64 / rate.max(1) as f64;
    if s < 60.0 { format!("{s:.1} s").replace('.', ",") } else { fmt_time(frames, rate, false) }
}

/// What the upload precheck will say, before exporting (estimated size).
fn upload_preview(fmt: ExportFormat, secs: f64, rate: u32, channels: u16, q: &crate::audioshare::api::Quality) -> Vec<String> {
    use crate::audioshare::queue::mmss;
    let mut v = Vec::new();
    let ext = fmt.ext();
    if !q.formats.is_empty() && !q.formats.iter().any(|f| f.eq_ignore_ascii_case(ext)) {
        v.push(format!("Formaat {} wordt niet geaccepteerd", ext.to_uppercase()));
    }
    let bytes = match fmt {
        ExportFormat::Mp3 => secs * 32_000.0,
        ExportFormat::Flac => secs * rate as f64 * channels as f64 * 3.0 * 0.65,
        ExportFormat::Wav => secs * rate as f64 * channels as f64 * 3.0,
    };
    let max = q.max_file_mb.unwrap_or(200) as f64;
    if bytes / 1_048_576.0 > max {
        v.push(format!("Wordt ca. {:.0} MB, maximaal {:.0} MB", bytes / 1_048_576.0, max));
    }
    if let Some(min) = q.min_file_mb {
        if bytes / 1_048_576.0 < min as f64 {
            v.push(format!("Wordt ca. {:.0} MB, minimaal {} MB", bytes / 1_048_576.0, min));
        }
    }
    if let Some(min) = q.min_duration_sec {
        if secs + 0.5 < min as f64 {
            v.push(format!("Te kort: {} (minimaal {})", mmss(secs), mmss(min as f64)));
        }
    }
    if let Some(maxd) = q.max_duration_sec {
        if secs > maxd as f64 + 0.5 {
            v.push(format!("Te lang: {} (maximaal {})", mmss(secs), mmss(maxd as f64)));
        }
    }
    if !q.samplerates.is_empty() && !q.samplerates.contains(&rate) {
        v.push(format!("Samplerate {rate} Hz wordt niet geaccepteerd"));
    }
    v
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    In,
    Out,
    Pan(f64),
    Select(u64),
    /// Edge of cut `i`: false = start, true = end.
    CutEdge(usize, bool),
    /// Edge of the current selection: false = start, true = end.
    SelEdge(bool),
    /// Dragging the playhead (scrubbing).
    Playhead,
}

struct Editor {
    /// The WAV the editor works on (the recording, or a working copy of an MP3/FLAC).
    path: PathBuf,
    /// The file the user opened.
    origin: PathBuf,
    fmt: WavFormat,
    frames: u64,
    peaks: Peaks,
    reader: Option<WavReader>,
    view_start: f64,
    view_len: f64,
    in_pt: u64,
    out_pt: u64,
    playhead: u64,
    drag: Option<Drag>,
    cache: Option<(u64, u64, Vec<f32>)>,
    export_name: String,
    /// True once the user typed an export name themselves.
    name_edited: bool,
    meta: Meta,
    form: FormState,
    /// Cut-out ranges, sorted and non-overlapping.
    cuts: Vec<(u64, u64)>,
    /// Range selected by dragging (not cut yet).
    sel: Option<(u64, u64)>,
    /// Index of a clicked cut (to listen to or restore).
    sel_cut: Option<usize>,
    undo: Vec<Vec<(u64, u64)>>,
}

impl Editor {
    fn new(path: PathBuf, origin: PathBuf, fmt: WavFormat, frames: u64, peaks: Peaks, meta: Meta) -> Editor {
        let stem = origin.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Editor {
            reader: WavReader::open(&path).ok(),
            path,
            origin,
            fmt,
            frames,
            peaks,
            view_start: 0.0,
            view_len: frames.max(1) as f64,
            in_pt: 0,
            out_pt: frames,
            playhead: 0,
            drag: None,
            cache: None,
            export_name: meta.export_name().unwrap_or_else(|| format!("{stem}_bewerkt")),
            name_edited: false,
            meta,
            form: FormState::default(),
            cuts: Vec::new(),
            sel: None,
            sel_cut: None,
            undo: Vec::new(),
        }
    }

    /// The parts of [a, b) that are not cut out.
    fn keep_segments(&self, a: u64, b: u64) -> Vec<(u64, u64)> {
        let mut v = Vec::new();
        let mut pos = a;
        for &(ca, cb) in &self.cuts {
            if cb <= pos {
                continue;
            }
            if ca >= b {
                break;
            }
            if ca > pos {
                v.push((pos, ca));
            }
            pos = pos.max(cb);
        }
        if pos < b {
            v.push((pos, b));
        }
        v
    }

    /// What to play when starting at `f`: up to UIT (or the end when past it), skipping cuts.
    fn play_from(&self, f: u64) -> Vec<(u64, u64)> {
        let end = if f < self.out_pt { self.out_pt } else { self.frames };
        self.keep_segments(f, end)
    }

    fn cut_at(&self, f: u64) -> Option<usize> {
        self.cuts.iter().position(|&(a, b)| f >= a && f < b)
    }

    fn add_cut(&mut self, a: u64, b: u64) {
        if b <= a {
            return;
        }
        self.undo.push(self.cuts.clone());
        self.cuts.push((a, b));
        self.cuts.sort();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for c in self.cuts.drain(..) {
            match merged.last_mut() {
                Some(last) if c.0 <= last.1 => last.1 = last.1.max(c.1),
                _ => merged.push(c),
            }
        }
        self.cuts = merged;
        self.sel_cut = None;
        if self.cut_at(self.playhead).is_some() {
            self.playhead = b;
        }
    }

    fn restore_cut(&mut self, i: usize) {
        if i < self.cuts.len() {
            self.undo.push(self.cuts.clone());
            self.cuts.remove(i);
        }
        self.sel_cut = None;
    }

    fn undo(&mut self) {
        if let Some(c) = self.undo.pop() {
            self.cuts = c;
            self.sel_cut = None;
        }
    }

    fn rate(&self) -> u32 {
        self.fmt.sample_rate
    }

    fn clamp_view(&mut self) {
        let total = self.frames.max(1) as f64;
        let min_len = 64.0f64.min(total);
        self.view_len = self.view_len.clamp(min_len, total);
        self.view_start = self.view_start.clamp(0.0, (total - self.view_len).max(0.0));
    }

    fn zoom(&mut self, factor: f64, anchor_frame: f64) {
        let rel = (anchor_frame - self.view_start) / self.view_len;
        self.view_len *= factor;
        self.clamp_view();
        self.view_start = anchor_frame - rel * self.view_len;
        self.clamp_view();
    }

    fn show_frame(&mut self, f: u64) {
        let f = f as f64;
        if f < self.view_start || f > self.view_start + self.view_len {
            self.view_start = f - self.view_len * 0.25;
            self.clamp_view();
        }
    }

    /// Samples for the visible range when zoomed in below the peak resolution.
    fn samples_for_view(&mut self) -> Option<(u64, &[f32])> {
        let a = self.view_start.floor().max(0.0) as u64;
        let b = ((self.view_start + self.view_len).ceil() as u64 + 2).min(self.frames);
        let ok = matches!(&self.cache, Some((ca, cb, _)) if *ca <= a && *cb >= b);
        if !ok {
            let r = self.reader.as_mut()?;
            let margin = (b - a) / 2;
            let ca = a.saturating_sub(margin);
            let cb = (b + margin).min(self.frames);
            let mut v = Vec::new();
            r.read_f32(ca, (cb - ca) as usize, &mut v).ok()?;
            self.cache = Some((ca, cb, v));
        }
        self.cache.as_ref().map(|(ca, _, v)| (*ca, v.as_slice()))
    }
}

struct Loading {
    path: PathBuf,
    progress: Arc<AtomicU32>,
    /// 0 = converting MP3/FLAC, 1 = reading the waveform
    stage: Arc<AtomicU32>,
    rx: Receiver<Result<(WavFormat, u64, Peaks, PathBuf), String>>,
}

/// What to upload to AudioShare once an export is ready.
struct UploadPlan {
    profile_id: String,
    profile_name: String,
    /// Studio: the local user who operated.
    operator: String,
    program_id: i64,
    program_name: String,
    meta: Meta,
    quality: crate::audioshare::api::Quality,
    duration_sec: f64,
    sample_rate: u32,
}

struct Exporting {
    upload: Option<UploadPlan>,
    dst: PathBuf,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<(), String>>,
    started: Instant,
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Record,
    Edit,
    Uploads,
}

pub struct App {
    settings: Settings,
    inputs: Vec<InputChoice>,
    outputs: Vec<(String, String)>,
    engine: Option<Engine>,
    engine_err: Option<String>,
    mode: Mode,
    rec_started: Option<Instant>,
    rec_path: Option<PathBuf>,
    confirm_stop: bool,
    confirm_quit: bool,
    allow_quit: bool,
    meter: Meter,
    editor: Option<Editor>,
    loading: Option<Loading>,
    exporting: Option<Exporting>,
    export_result: Option<Result<PathBuf, String>>,
    player: Player,
    show_settings: bool,
    /// Programme information on the recording screen.
    meta: Meta,
    form: FormState,
    new_program: String,
    toast: Option<(String, Instant, bool)>,
    free_bytes: Option<u64>,
    free_checked: Option<Instant>,
    aus: as_ui::AsUi,
    ipc_rx: Option<Receiver<PathBuf>>,
    title_shown: String,
    /// Upload job started from the editor's last export.
    last_job: Option<String>,
    /// Compact always-on-top window while recording.
    mini: bool,
    /// Profile the recording-screen meta was prepared for.
    meta_profile: Option<String>,
    /// JohnWorks logo (light version for the dark interface).
    logo: egui::TextureHandle,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> App {
        setup_style(&cc.egui_ctx);
        let settings = Settings::load();
        let mut app = App {
            settings,
            inputs: Vec::new(),
            outputs: Vec::new(),
            engine: None,
            engine_err: None,
            mode: Mode::Record,
            rec_started: None,
            rec_path: None,
            confirm_stop: false,
            confirm_quit: false,
            allow_quit: false,
            meter: Meter::default(),
            editor: None,
            loading: None,
            exporting: None,
            export_result: None,
            player: Player::default(),
            show_settings: false,
            meta: Meta { date: Meta::today(), ..Default::default() },
            form: FormState::default(),
            new_program: String::new(),
            toast: None,
            free_bytes: None,
            free_checked: None,
            aus: as_ui::AsUi::new(),
            ipc_rx: crate::ipc::listen(),
            title_shown: String::new(),
            last_job: None,
            mini: false,
            meta_profile: None,
            logo: {
                let img = egui::ColorImage::from_rgba_unmultiplied([LOGO_W, LOGO_H], include_bytes!("../res/johnworks-dark.rgba"));
                cc.egui_ctx.load_texture("johnworks", img, egui::TextureOptions::LINEAR)
            },
        };
        app.meter.reset();
        std::thread::spawn(crate::decode::clean_cache);
        app.refresh_devices();
        app.open_selected_input();
        if let Some(p) = std::env::args().nth(1) {
            app.open_path(PathBuf::from(p));
        } else {
            app.aus.auto_link();
        }
        app
    }

    fn refresh_devices(&mut self) {
        self.inputs = engine::list_inputs();
        self.outputs = player::list_outputs();
    }

    fn current_input(&self) -> Option<InputChoice> {
        if let Some(c) = self.inputs.iter().find(|c| c.id == self.settings.input_id) {
            return Some(c.clone());
        }
        let def = engine::default_input_id();
        self.inputs
            .iter()
            .find(|c| Some(&c.id) == def.as_ref())
            .or_else(|| self.inputs.iter().find(|c| !c.loopback))
            .cloned()
    }

    fn open_selected_input(&mut self) {
        if self.engine.as_ref().is_some_and(|e| e.is_recording()) {
            return;
        }
        self.engine = None;
        self.engine_err = None;
        self.meter.reset();
        match self.current_input() {
            None => self.engine_err = Some("Geen invoerapparaat gevonden.".into()),
            Some(choice) => match Engine::open(&choice) {
                Ok(e) => self.engine = Some(e),
                Err(e) => self.engine_err = Some(e),
            },
        }
    }

    fn is_recording(&self) -> bool {
        self.engine.as_ref().is_some_and(|e| e.is_recording())
    }

    fn toast(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), Instant::now(), error));
    }

    /// A file from the command line, a double-click or drag & drop.
    fn open_path(&mut self, p: PathBuf) {
        if !p.is_file() {
            return;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "audioshare" => self.aus.open_link(Some(&p)),
            "wav" | "mp3" | "flac" if self.loading.is_none() && self.exporting.is_none() => self.load_file(p),
            _ => {}
        }
    }

    fn start_recording(&mut self) {
        let Some(engine) = &self.engine else { return };
        let folder = PathBuf::from(&self.settings.record_folder);
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.toast(format!("Kan opnamemap niet maken: {e}"), true);
            return;
        }
        let base = if self.meta.program.trim().is_empty() { self.settings.studio_name.clone() } else { self.meta.program.clone() };
        let base = match safe_file_part(&base) {
            b if b.is_empty() => "Opname".to_string(),
            b => b,
        };
        let name = format!("{base}_{}.wav", chrono::Local::now().format("%Y-%m-%d_%H-%M-%S"));
        let path = folder.join(name);
        match engine.start_recording(&path) {
            Ok(()) => {
                self.meta.save(&path);
                self.rec_started = Some(Instant::now());
                self.rec_path = Some(path);
                engine::keep_awake(true);
            }
            Err(e) => self.toast(e, true),
        }
    }

    fn toggle_pause(&mut self) {
        let Some(e) = &self.engine else { return };
        if !e.is_recording() {
            return;
        }
        let now = !e.is_paused();
        e.set_paused(now);
        self.toast(if now { "Opname gepauzeerd — er wordt niets opgenomen tot je hervat" } else { "Opname hervat" }, false);
    }

    fn set_mini(&mut self, ctx: &egui::Context, on: bool) {
        if self.mini == on {
            return;
        }
        self.mini = on;
        use egui::ViewportCommand as V;
        if on {
            ctx.send_viewport_cmd(V::MinInnerSize(vec2(580.0, 100.0)));
            ctx.send_viewport_cmd(V::InnerSize(vec2(640.0, 100.0)));
            ctx.send_viewport_cmd(V::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        } else {
            ctx.send_viewport_cmd(V::WindowLevel(egui::WindowLevel::Normal));
            ctx.send_viewport_cmd(V::MinInnerSize(vec2(980.0, 640.0)));
            ctx.send_viewport_cmd(V::InnerSize(vec2(1280.0, 880.0)));
            ctx.send_viewport_cmd(V::Focus);
        }
    }

    /// Prepare the recording screen for whoever is signed in: their
    /// programme (if there's one, or the one they used last), today's date
    /// and the title AudioShare suggests.
    fn prepare_meta(&mut self) {
        // Editor: a maker with exactly one programme gets it filled in.
        if let (Some(ed), Some(prof)) = (self.editor.as_mut(), self.aus.active_profile()) {
            if ed.meta.program_id.is_none() && prof.programs.len() == 1 {
                let p = &prof.programs[0];
                ed.meta.program = p.name.clone();
                ed.meta.program_id = Some(p.id);
                ed.form.want_info = Some(p.id);
                if !ed.name_edited {
                    if let Some(n) = ed.meta.export_name() {
                        ed.export_name = n;
                    }
                }
            }
        }
        if self.is_recording() {
            return;
        }
        let active = self.aus.session_key();
        if active != self.meta_profile {
            self.meta_profile = active.clone();
            self.meta = Meta { date: Meta::today(), ..Default::default() };
            self.form = FormState::default();
        }
        let Some(prof) = self.aus.active_profile() else { return };
        if self.meta.program_id.is_some_and(|id| prof.programs.iter().any(|p| p.id == id)) {
            return;
        }
        let pick = if prof.programs.len() == 1 {
            Some(&prof.programs[0])
        } else {
            self.settings.last_program.get(&prof.id).and_then(|id| prof.programs.iter().find(|p| p.id == *id))
        };
        if let Some(p) = pick {
            self.meta.program = p.name.clone();
            self.meta.program_id = Some(p.id);
            self.form.want_info = Some(p.id);
        } else if self.meta.program_id.is_some() {
            self.meta.program.clear();
            self.meta.program_id = None;
        }
    }

    /// Who is recording, and for which programme (replaces the full form;
    /// title, date and description are checked at export).
    fn maker_card(&mut self, ui: &mut egui::Ui, recording: bool) {
        egui::Frame::NONE
            .fill(PANEL)
            .corner_radius(10.0)
            .stroke(Stroke::new(1.0, LINE))
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                let w = ui.available_width() - 4.0;
                ui.vertical(|ui| {
                    ui.set_width(w);
                    ui.set_min_height(96.0);
                    if !self.aus.configured() {
                        ui.label(egui::RichText::new("Programmagegevens").size(14.0).strong());
                        ui.add_space(4.0);
                        hint_line(ui, "Programma, uitzenddag en titel vul je in bij het exporteren (tabblad Bewerken).");
                        return;
                    }
                    let Some(prof) = self.aus.active_profile() else {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Niet ingelogd").size(16.0).strong().color(AMBER));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ibutton(ui, None, "Inloggen", true, Some(PURPLE)).clicked() {
                                    self.aus.unlock = Some(Default::default());
                                }
                            });
                        });
                        ui.add_space(4.0);
                        hint_line(ui, "Log in, dan staat je programma klaar en kun je uploaden. Zonder inloggen kun je opnemen, bewerken en opslaan.");
                        return;
                    };
                    let name = prof.display_name.clone();
                    let admin = prof.is_admin();
                    let pid = prof.id.clone();
                    let programs = prof.programs.clone();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Ingelogd als").color(DIM));
                        ui.label(egui::RichText::new(&name).size(16.0).strong());
                        if admin {
                            ui.label(egui::RichText::new("beheerder").size(11.5).color(PURPLE));
                        }
                        if !recording {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("Uitloggen").clicked() {
                                    self.aus.sign_out();
                                }
                            });
                        }
                    });
                    ui.add_space(6.0);
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Programma").color(DIM));
                        match programs.len() {
                            0 => {
                                ui.label(egui::RichText::new("Geen programma's toegewezen in AudioShare").color(AMBER));
                            }
                            1 => {
                                ui.label(egui::RichText::new(&programs[0].name).size(15.0).strong().color(GREEN));
                            }
                            _ => {
                                let src = ProgramSource::Remote(&programs);
                                let wdt = (ui.available_width() - 8.0).clamp(160.0, 340.0);
                                changed = program_picker(ui, &mut self.meta, &mut self.settings, &mut self.form, "rec_prog", wdt, &src);
                            }
                        }
                    });
                    if changed {
                        if let Some(id) = self.meta.program_id {
                            self.settings.last_program.insert(pid.clone(), id);
                            self.settings.save();
                        }
                        if let (true, Some(p)) = (recording, &self.rec_path) {
                            self.meta.save(p);
                        }
                    }
                    ui.add_space(4.0);
                    use chrono::Datelike;
                    let date = chrono::NaiveDate::parse_from_str(&self.meta.date, "%Y-%m-%d")
                        .map(|d| format!("{} {} {}", WEEKDAYS_NL[d.weekday().num_days_from_monday() as usize], d.day(), MONTHS_NL[d.month0() as usize]))
                        .unwrap_or_else(|_| self.meta.date.clone());
                    let title = if self.meta.title.trim().is_empty() { "titel volgt bij export".to_string() } else { self.meta.title.clone() };
                    ui.add(
                        egui::Label::new(egui::RichText::new(format!("Uitzenddag {date} · {title}")).color(DIM).size(12.5)).truncate(),
                    )
                    .on_hover_text("Uitzenddag, titel en beschrijving kun je na de opname nog aanpassen in Bewerken");
                });
            });
    }

    /// Compact view: REC, time, small meter, pause, stop, back to big.
    fn mini_screen(&mut self, ui: &mut egui::Ui) {
        let recording = self.is_recording();
        let paused = self.engine.as_ref().is_some_and(|e| e.is_paused());
        let (rate, frames) = self
            .engine
            .as_ref()
            .map(|e| (e.shared.sample_rate, e.shared.rec_frames.load(Ordering::Relaxed)))
            .unwrap_or((48000, 0));
        ui.horizontal_centered(|ui| {
            // status dot
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            let t = ui.input(|i| i.time);
            let col = if paused {
                if (t * 2.0) as i64 % 2 == 0 { AMBER } else { AMBER.gamma_multiply(0.3) }
            } else if recording {
                RED.gamma_multiply(0.6 + 0.4 * (t * 3.0).sin().abs() as f32)
            } else {
                DIM
            };
            ui.painter().circle_filled(r.center(), 7.0, col);
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(if paused { "GEPAUZEERD" } else if recording { "REC" } else { "GEEN OPNAME" })
                        .size(11.0)
                        .strong()
                        .color(if paused { AMBER } else if recording { RED } else { DIM }),
                );
                ui.label(
                    egui::RichText::new(fmt_time(if recording { frames } else { 0 }, rate, false))
                        .font(FontId::monospace(30.0))
                        .color(if recording && !paused { TEXT } else { DIM }),
                );
            });
            ui.add_space(6.0);
            // tiny horizontal meter
            let (mr, _) = ui.allocate_exact_size(vec2(110.0, 30.0), Sense::hover());
            let p = ui.painter();
            for (ch, y) in [(0usize, mr.top() + 4.0), (1usize, mr.top() + 17.0)] {
                let bar = Rect::from_min_size(pos2(mr.left(), y), vec2(mr.width(), 9.0));
                p.rect_filled(bar, 2.0, PANEL2);
                let level = self.meter.level[ch];
                let frac = ((level - METER_MIN) / -METER_MIN).clamp(0.0, 1.0);
                let c = if level > -1.0 { RED } else if level > -9.0 { AMBER } else { GREEN };
                p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar.height())), 2.0, c);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ibutton(ui, Some(Icon::Fit), "Groot", true, None).on_hover_text("Terug naar het volledige venster").clicked() {
                    self.set_mini(ui.ctx(), false);
                }
                if recording {
                    if self.confirm_stop {
                        if ibutton(ui, None, "Nee", true, None).clicked() {
                            self.confirm_stop = false;
                        }
                        if ibutton(ui, Some(Icon::Stop), "Ja, stop", true, Some(RED)).clicked() {
                            self.confirm_stop = false;
                            self.set_mini(ui.ctx(), false);
                            self.stop_recording();
                        }
                    } else {
                        if ibutton(ui, Some(Icon::Stop), "Stop", true, Some(RED)).on_hover_text("Opname stoppen").clicked() {
                            self.confirm_stop = true;
                        }
                        let (icon, label) = if paused { (Icon::Play, "Hervat") } else { (Icon::Pause, "Pauze") };
                        if ibutton(ui, Some(icon), label, true, if paused { Some(AMBER) } else { None }).clicked() {
                            self.toggle_pause();
                        }
                    }
                } else if ibutton(ui, None, "● Opnemen", self.engine.is_some(), Some(RED)).clicked() {
                    self.start_recording();
                }
            });
        });
    }

    fn stop_recording(&mut self) {
        let Some(engine) = &self.engine else { return };
        let res = engine.stop_recording();
        let (rate, channels) = (engine.shared.sample_rate, engine.shared.channels as u16);
        engine::keep_awake(false);
        self.rec_started = None;
        if let Some(e) = &res.error {
            self.toast(e.clone(), true);
        }
        if let (Some(path), Some(peaks)) = (res.path, res.peaks) {
            if res.frames > 0 {
                let fmt = WavFormat {
                    sample_rate: rate,
                    channels,
                    bits: 24,
                    float: false,
                };
                self.player.stop();
                self.meta.save(&path);
                self.settings.remember(&self.meta);
                self.settings.save();
                self.editor = Some(Editor::new(path.clone(), path.clone(), fmt, res.frames, peaks, self.meta.clone()));
                self.export_result = None;
                self.mode = Mode::Edit;
                self.toast(format!("Opname opgeslagen: {}", path.display()), false);
            }
        }
    }

    fn load_file(&mut self, path: PathBuf) {
        self.player.stop();
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let p2 = path.clone();
        let prog = progress.clone();
        let stage = Arc::new(AtomicU32::new(0));
        let st2 = stage.clone();
        std::thread::spawn(move || {
            let res = (|| {
                let work = if crate::decode::needs_decoding(&p2) {
                    crate::decode::working_copy(&p2, &prog)?
                } else {
                    p2.clone()
                };
                st2.store(1, Ordering::Relaxed);
                prog.store(0, Ordering::Relaxed);
                let mut r = WavReader::open(&work).map_err(|e| format!("Kan bestand niet openen: {e}"))?;
                let peaks = Peaks::from_reader(&mut r, &prog, &AtomicBool::new(false)).map_err(|e| e.to_string())?;
                Ok((r.fmt, r.frames, peaks, work))
            })();
            let _ = tx.send(res);
        });
        self.loading = Some(Loading { path, progress, stage, rx });
        self.mode = Mode::Edit;
    }

    fn start_export(&mut self) {
        let Some(ed) = &self.editor else { return };
        // Programme, date and title are only required for a logged-in maker
        // (they go to AudioShare); without login it's a plain save.
        let uploading = self.aus.active_profile().is_some() && self.settings.auto_upload;
        let missing = if uploading { ed.meta.missing() } else { Vec::new() };
        if !missing.is_empty() {
            self.export_result = Some(Err(format!("Vul voor het uploaden eerst in: {} (of zet uploaden uit)", missing.join(", "))));
            return;
        }
        let fmt = self.settings.export_format;
        let name = ed.export_name.trim();
        let name = if name.is_empty() { "export" } else { name };
        let dir = ed.origin.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut dst = dir.join(format!("{name}.{}", fmt.ext()));
        let mut n = 2;
        while dst.exists() {
            dst = dir.join(format!("{name} ({n}).{}", fmt.ext()));
            n += 1;
        }
        if dst == ed.origin || dst == ed.path {
            self.export_result = Some(Err("Kies een andere naam dan het origineel".into()));
            return;
        }
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let src = ed.path.clone();
        let segments = ed.keep_segments(ed.in_pt, ed.out_pt);
        let fade = self.settings.fade_ms;
        let tags = ed.meta.tags(&self.settings.studio_name);
        ed.meta.save(&ed.origin);
        self.settings.remember(&ed.meta);
        self.settings.save();
        let upload = self.upload_plan();
        let (pr, ca, d2) = (progress.clone(), cancel.clone(), dst.clone());
        std::thread::spawn(move || {
            let job = ExportJob { src: &src, dst: &d2, format: fmt, segments, fade_ms: fade, crossfade_ms: CROSSFADE_MS, tags };
            let _ = tx.send(export::export(&job, &pr, &ca));
        });
        self.export_result = None;
        self.last_job = None;
        self.exporting = Some(Exporting { upload, dst, progress, cancel, rx, started: Instant::now() });
    }

    /// Upload after export? Only with a signed-in maker and an AudioShare programme.
    fn upload_plan(&self) -> Option<UploadPlan> {
        if !self.settings.auto_upload {
            return None;
        }
        let ed = self.editor.as_ref()?;
        let pid = ed.meta.program_id?;
        let prof = self.aus.active_profile()?;
        let prog = prof.programs.iter().find(|p| p.id == pid)?;
        let rate = ed.rate();
        let frames = export::output_frames(&ed.keep_segments(ed.in_pt, ed.out_pt), CROSSFADE_MS as u64 * rate as u64 / 1000);
        Some(UploadPlan {
            profile_id: prof.id.clone(),
            profile_name: prof.display_name.clone(),
            operator: self.aus.operator(),
            program_id: pid,
            program_name: prog.name.clone(),
            meta: ed.meta.clone(),
            quality: prog.quality.clone(),
            duration_sec: frames as f64 / rate as f64,
            sample_rate: rate,
        })
    }

    fn poll_background(&mut self) {
        if let Some(l) = &self.loading {
            if let Ok(res) = l.rx.try_recv() {
                let path = l.path.clone();
                self.loading = None;
                match res {
                    Ok((fmt, frames, peaks, work)) => {
                        if frames == 0 {
                            self.toast("Dit bestand bevat geen audio", true);
                        } else {
                            let meta = Meta::load(&path).unwrap_or_else(|| Meta { date: Meta::today(), ..Default::default() });
                            self.editor = Some(Editor::new(work, path, fmt, frames, peaks, meta));
                            self.export_result = None;
                        }
                    }
                    Err(e) => self.toast(e, true),
                }
            }
        }
        if let Some(x) = &mut self.exporting {
            if let Ok(res) = x.rx.try_recv() {
                let dst = x.dst.clone();
                let plan = x.upload.take();
                self.exporting = None;
                if let (Ok(()), Some(p)) = (&res, plan) {
                    let job = crate::audioshare::queue::Job {
                        profile_id: p.profile_id,
                        profile_name: p.profile_name,
                        operator: p.operator,
                        program_id: p.program_id,
                        program_name: p.program_name,
                        date: p.meta.date.clone(),
                        title: p.meta.title.trim().to_string(),
                        description: p.meta.description.trim().to_string(),
                        file: dst.clone(),
                        duration_sec: p.duration_sec,
                        sample_rate: p.sample_rate,
                        quality: p.quality,
                        ..Default::default()
                    };
                    self.last_job = Some(job.id.clone());
                    self.aus.queue.add(job);
                }
                self.export_result = Some(res.map(|_| dst));
            }
        }
        self.player.tick();
    }

    // -----------------------------------------------------------------------
    // Top bar
    // -----------------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let (r, _) = ui.allocate_exact_size(vec2(22.0, 32.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 9.0, RED);
            ui.painter().circle_stroke(r.center(), 9.0, Stroke::new(2.0, TEXT.gamma_multiply(0.9)));
            ui.label(egui::RichText::new("StudioRecorder").size(18.0).strong().color(TEXT));
            ui.label(egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).size(12.0).color(DIM));
            ui.add_space(24.0);
            let rec = self.is_recording();
            let open_jobs = self.aus.open_count();
            let uploads_label = if open_jobs > 0 { format!("Uploads ({open_jobs})") } else { "Uploads".to_string() };
            for (m, label) in [(Mode::Record, "Opnemen"), (Mode::Edit, "Bewerken"), (Mode::Uploads, uploads_label.as_str())] {
                let selected = self.mode == m;
                let text = if m == Mode::Record && rec && !selected { "Opnemen  ● REC" } else { label };
                let font = FontId::proportional(15.0);
                let galley = ui.painter().layout_no_wrap(text.to_string(), font, TEXT);
                let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 28.0, 34.0), Sense::click());
                let col = if selected { TEXT } else if resp.hovered() { TEXT.gamma_multiply(0.85) } else { DIM };
                let col = if m == Mode::Record && rec && !selected { RED } else { col };
                if selected {
                    ui.painter().rect_filled(rect, 6.0, PANEL2);
                    ui.painter().line_segment(
                        [pos2(rect.left() + 10.0, rect.bottom() - 1.0), pos2(rect.right() - 10.0, rect.bottom() - 1.0)],
                        Stroke::new(2.0, match m {
                            Mode::Record => RED,
                            Mode::Edit => WAVE,
                            Mode::Uploads => GREEN,
                        }),
                    );
                }
                ui.painter().galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, col);
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.mode = m;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                let may = self.aus.admin_unlocked();
                let r = ibutton(ui, Some(Icon::Gear), "Instellingen", true, None);
                let r = if may { r } else { r.on_hover_text("Alleen voor de beheerder") };
                if r.clicked() {
                    if may {
                        self.refresh_devices();
                        self.show_settings = true;
                    } else {
                        self.toast("Alleen een beheerder kan instellingen wijzigen — log in als beheerder", true);
                        self.aus.unlock = Some(Default::default());
                    }
                }
                ui.add_space(10.0);
                self.aus.top_chip(ui);
            });
        });
    }

    // -----------------------------------------------------------------------
    // Record screen
    // -----------------------------------------------------------------------

    fn record_screen(&mut self, ui: &mut egui::Ui) {
        let recording = self.is_recording();

        // Device row
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Invoer").color(DIM));
            let current = self.current_input();
            let current_name = current.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "—".into());
            let mut chosen: Option<String> = None;
            let may_change = self.aus.admin_unlocked();
            ui.add_enabled_ui(!recording && may_change, |ui| {
                egui::ComboBox::from_id_salt("input")
                    .width(360.0)
                    .selected_text(current_name)
                    .show_ui(ui, |ui| {
                        for c in &self.inputs {
                            let sel = current.as_ref().is_some_and(|x| x.id == c.id);
                            if ui.selectable_label(sel, &c.name).clicked() {
                                chosen = Some(c.id.clone());
                            }
                        }
                    })
                    .response
                    .on_disabled_hover_text(if may_change {
                        "Stop eerst de opname om van invoer te wisselen"
                    } else {
                        "Alleen een beheerder kan de invoer wijzigen"
                    });
                if ibutton(ui, None, "Vernieuwen", !recording, None)
                    .on_hover_text("Apparatenlijst opnieuw inlezen")
                    .clicked()
                {
                    self.refresh_devices();
                    self.open_selected_input();
                }
            });
            if let Some(id) = chosen {
                self.settings.input_id = id;
                self.settings.save();
                self.open_selected_input();
            }
            if let Some(e) = &self.engine {
                let ch = match e.shared.channels {
                    1 => "mono",
                    _ => "stereo",
                };
                ui.label(
                    egui::RichText::new(format!("{} kHz · 24-bit · {ch}", e.shared.sample_rate as f32 / 1000.0)).color(DIM),
                );
            }
        });
        ui.add_space(8.0);

        // Warnings
        let mut warnings: Vec<(String, Color32)> = Vec::new();
        if let Some(e) = &self.engine_err {
            warnings.push((e.clone(), RED));
        }
        if let Some(e) = &self.engine {
            if let Some(err) = e.shared.error.lock().ok().and_then(|g| g.clone()) {
                warnings.push((format!("Probleem met invoerapparaat: {err}"), RED));
            }
            if let Some(err) = e.shared.write_error.lock().ok().and_then(|g| g.clone()) {
                warnings.push((err, RED));
            }
            let drops = e.shared.dropouts.load(Ordering::Relaxed);
            if recording && drops > 0 {
                warnings.push((format!("{drops} hapering(en) in de audio gedetecteerd"), AMBER));
            }
            let silent = e.shared.last_signal.lock().map(|t| t.elapsed()).unwrap_or_default();
            if recording && !e.is_paused() && silent > Duration::from_secs(10) {
                warnings.push((format!("Geen signaal sinds {} s — staat de goede invoer open?", silent.as_secs()), AMBER));
            }
        }
        if let Some(free) = self.free_bytes {
            if free < 2_000_000_000 {
                warnings.push((format!("Weinig schijfruimte: nog {}", gb(free)), RED));
            }
        }
        for (w, col) in &warnings {
            egui::Frame::NONE
                .fill(col.gamma_multiply(0.18))
                .stroke(Stroke::new(1.0, col.gamma_multiply(0.7)))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new(w).color(TEXT));
                });
            ui.add_space(4.0);
        }

        // Timer + big button
        let (rate, frames) = self
            .engine
            .as_ref()
            .map(|e| (e.shared.sample_rate, e.shared.rec_frames.load(Ordering::Relaxed)))
            .unwrap_or((48000, 0));
        ui.add_space(6.0);
        let paused = self.engine.as_ref().is_some_and(|e| e.is_paused());
        ui.horizontal_top(|ui| {
            let h = 120.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(h, h), Sense::click());
            let can = self.engine.is_some();
            let hovered = resp.hovered() && can;
            let p = ui.painter();
            let c = rect.center();
            if recording {
                let t = self.rec_started.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
                let pulse = if paused { 0.0 } else { 0.5 + 0.5 * (t * 3.0).sin() };
                p.circle_filled(c, h * 0.5, RED.gamma_multiply(0.15 + 0.15 * pulse));
                p.circle_filled(c, h * 0.40, if hovered { RED.linear_multiply(1.2) } else { RED });
                p.rect_filled(Rect::from_center_size(c, vec2(h * 0.26, h * 0.26)), 4.0, TEXT);
            } else {
                p.circle_filled(c, h * 0.5, if hovered { PANEL2.linear_multiply(1.3) } else { PANEL2 });
                p.circle_stroke(c, h * 0.5 - 1.0, Stroke::new(2.0, LINE));
                p.circle_filled(c, h * 0.24, if can { RED } else { RED_DARK });
            }
            let tip = if recording { "Opname stoppen" } else { "Opname starten" };
            if can && resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                if recording {
                    self.confirm_stop = true;
                } else {
                    self.start_recording();
                }
            }
            // Pause / resume
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_space(25.0);
                if pause_button(ui, 70.0, recording, paused) {
                    self.toggle_pause();
                }
            });
            ui.add_space(16.0);
            ui.vertical(|ui| {
                ui.set_width(310.0);
                ui.add_space(8.0);
                let (label, col) = if paused {
                    let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
                    ("GEPAUZEERD", if blink { AMBER } else { AMBER.gamma_multiply(0.4) })
                } else if recording {
                    ("OPNAME LOOPT", RED)
                } else {
                    ("KLAAR VOOR OPNAME", DIM)
                };
                ui.label(egui::RichText::new(label).size(14.0).strong().color(col));
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(fmt_time(if recording { frames } else { 0 }, rate, false))
                            .font(FontId::monospace(60.0))
                            .color(if recording && !paused { TEXT } else { DIM }),
                    )
                    .wrap_mode(egui::TextWrapMode::Extend),
                );
                if recording {
                    if let Some(p) = &self.rec_path {
                        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        ui.add(egui::Label::new(egui::RichText::new(name).color(DIM).size(12.5)).truncate())
                            .on_hover_text(p.display().to_string());
                    }
                } else {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("Opslaan in: {}", self.settings.record_folder)).color(DIM).size(12.5),
                        )
                        .truncate(),
                    );
                }
                ui.add_space(4.0);
                if ibutton(ui, Some(Icon::Fit), "Klein venster", true, None)
                    .on_hover_text("Compacte balk die bovenop blijft, zodat je ondertussen iets anders op deze pc kunt doen")
                    .clicked()
                {
                    self.set_mini(ui.ctx(), true);
                }
            });
            ui.add_space(16.0);
            self.maker_card(ui, recording);
        });
        ui.add_space(12.0);

        // Live waveform: last 30 seconds + overview of the whole take
        let avail = ui.available_size();
        let live_h = (avail.y - 90.0).clamp(120.0, 520.0);
        let (live_rect, _) = ui.allocate_exact_size(vec2(avail.x, live_h), Sense::hover());
        let p = ui.painter_at(live_rect);
        p.rect_filled(live_rect, 8.0, PANEL);
        let inner = live_rect.shrink2(vec2(0.0, 6.0));
        let ruler_h = 18.0;
        let wave_rect = Rect::from_min_max(pos2(inner.left(), inner.top() + ruler_h), inner.max);
        let window = 30.0 * rate as f64;
        if let Some(e) = &self.engine {
            if recording {
                if let Ok(pk) = e.shared.peaks.lock() {
                    // Grow from the left until the window is full, then scroll.
                    let total = pk.total_frames() as f64;
                    let start = (total - window * 0.92).max(0.0);
                    draw_ruler(&p, Rect::from_min_size(inner.min, vec2(inner.width(), ruler_h)), start, window, rate);
                    draw_wave(&p, wave_rect, &WaveSource::Peaks(&pk), pk.channels, start, window, &|_| WAVE);
                    let x = wave_rect.left() + ((total - start) / window) as f32 * wave_rect.width();
                    let paused = e.is_paused();
                    p.line_segment([pos2(x, wave_rect.top()), pos2(x, wave_rect.bottom())], Stroke::new(1.5, if paused { AMBER } else { RED }));
                    if paused {
                        p.rect_filled(wave_rect, 0.0, BG.gamma_multiply(0.55));
                        p.text(wave_rect.center(), Align2::CENTER_CENTER, "Gepauzeerd — klik op hervatten om verder op te nemen", FontId::proportional(18.0), AMBER);
                    }
                }
            } else {
                p.text(
                    live_rect.center(),
                    Align2::CENTER_CENTER,
                    "Druk op de rode knop om de opname te starten",
                    FontId::proportional(16.0),
                    DIM,
                );
            }
        }
        ui.add_space(8.0);
        let (ov_rect, _) = ui.allocate_exact_size(vec2(avail.x, 46.0), Sense::hover());
        let p = ui.painter_at(ov_rect);
        p.rect_filled(ov_rect, 6.0, PANEL);
        if let (true, Some(e)) = (recording, &self.engine) {
            if let Ok(pk) = e.shared.peaks.lock() {
                let total = pk.total_frames().max(1) as f64;
                draw_wave(&p, ov_rect.shrink(3.0), &WaveSource::Peaks(&pk), 1, 0.0, total, &|_| WAVE.gamma_multiply(0.7));
            }
            p.text(ov_rect.left_top() + vec2(8.0, 4.0), Align2::LEFT_TOP, "hele opname", FontId::proportional(10.5), DIM);
        }

        // Footer
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if let Some(free) = self.free_bytes {
                let bps = rate as f64 * 3.0 * self.engine.as_ref().map(|e| e.shared.channels).unwrap_or(2) as f64;
                let hours = free as f64 / bps / 3600.0;
                ui.label(egui::RichText::new(format!("Vrije schijfruimte: {} (≈ {:.0} uur opnametijd)", gb(free), hours)).color(DIM).size(12.0));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                logo(ui, &self.logo, 15.0).on_hover_text(concat!("StudioRecorder v", env!("CARGO_PKG_VERSION"), " — JohnWorks"));
            });
        });
    }

    // -----------------------------------------------------------------------
    // Edit screen
    // -----------------------------------------------------------------------

    fn edit_screen(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ibutton(ui, Some(Icon::Folder), "Bestand openen…", self.loading.is_none(), None).clicked() {
                let mut dlg = rfd::FileDialog::new().add_filter("Audio (WAV, MP3, FLAC)", &["wav", "mp3", "flac", "WAV", "MP3", "FLAC"]);
                if let Some(ed) = &self.editor {
                    if let Some(d) = ed.origin.parent() {
                        dlg = dlg.set_directory(d);
                    }
                } else {
                    dlg = dlg.set_directory(&self.settings.record_folder);
                }
                if let Some(p) = dlg.pick_file() {
                    self.load_file(p);
                }
            }
            if let Some(ed) = &self.editor {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(ed.origin.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} kHz · {} · {}",
                        fmt_time(ed.frames, ed.rate(), false),
                        ed.rate() as f32 / 1000.0,
                        match crate::decode::extension(&ed.origin).as_str() {
                            "mp3" => "MP3".to_string(),
                            "flac" => format!("FLAC {}-bit", ed.fmt.bits),
                            _ => format!("{}-bit", ed.fmt.bits),
                        },
                        if ed.fmt.channels == 1 { "mono" } else { "stereo" }
                    ))
                    .color(DIM),
                );
            }
        });
        ui.add_space(8.0);

        if let Some(l) = &self.loading {
            let pr = l.progress.load(Ordering::Relaxed) as f32 / 1000.0;
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                let what = if l.stage.load(Ordering::Relaxed) == 0 && crate::decode::needs_decoding(&l.path) {
                    "Omzetten naar werkkopie"
                } else {
                    "Golfvorm inlezen"
                };
                ui.label(egui::RichText::new(format!("{what}: {}", l.path.display())).color(DIM));
                ui.add(egui::ProgressBar::new(pr).desired_width(400.0).show_percentage());
            });
            return;
        }
        if self.editor.is_none() {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("Nog geen opname geladen").size(20.0).color(TEXT));
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Na het stoppen van een opname verschijnt die hier automatisch.\nJe kunt ook een bestaand WAV-, MP3- of FLAC-bestand openen of op dit venster slepen.",
                    )
                    .color(DIM),
                );
            });
            return;
        }

        // Keyboard shortcuts
        let typing = ui.ctx().wants_keyboard_input();
        let (k_space, k_i, k_o, k_home, k_end, k_plus, k_minus, k_del, k_undo, k_esc) = ui.input(|i| {
            (
                i.key_pressed(Key::Space),
                i.key_pressed(Key::I),
                i.key_pressed(Key::O),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                i.key_pressed(Key::Minus),
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.modifiers.command && i.key_pressed(Key::Z),
                i.key_pressed(Key::Escape),
            )
        });

        let playing = self.player.is_playing();
        let output_id = (!self.settings.output_id.is_empty()).then(|| self.settings.output_id.clone());
        let ed = self.editor.as_mut().unwrap();
        if playing && ed.drag != Some(Drag::Playhead) {
            ed.playhead = self.player.position().min(ed.frames);
            let ph = ed.playhead as f64;
            if ph > ed.view_start + ed.view_len || ph < ed.view_start {
                ed.view_start = ph - ed.view_len * 0.1;
                ed.clamp_view();
            }
        }
        let rate = ed.rate();
        let listen = 3 * rate as u64; // "beluister" length
        let mut play_req: Option<Vec<(u64, u64)>> = None;
        let mut stop_req = false;

        if !typing {
            if k_space {
                if playing {
                    stop_req = true;
                } else {
                    play_req = Some(ed.play_from(ed.playhead));
                }
            }
            if k_i {
                ed.in_pt = ed.playhead.min(ed.out_pt.saturating_sub(1));
            }
            if k_o {
                ed.out_pt = ed.playhead.max(ed.in_pt + 1).min(ed.frames);
            }
            if k_home {
                ed.playhead = ed.in_pt;
                ed.show_frame(ed.in_pt);
            }
            if k_end {
                ed.playhead = ed.out_pt;
                ed.show_frame(ed.out_pt);
            }
            if k_plus {
                let a = ed.playhead as f64;
                ed.zoom(0.5, a);
            }
            if k_minus {
                let a = ed.playhead as f64;
                ed.zoom(2.0, a);
            }
            if k_del {
                if let Some((a, b)) = ed.sel.take() {
                    ed.add_cut(a, b);
                }
            }
            if k_undo {
                ed.undo();
            }
            if k_esc {
                ed.sel = None;
                ed.sel_cut = None;
            }
        }

        // Main waveform
        let avail = ui.available_size();
        let wave_h = (avail.y - 500.0).clamp(100.0, 520.0);
        let (outer, resp) = ui.allocate_exact_size(vec2(avail.x, wave_h), Sense::click_and_drag());
        let p = ui.painter_at(outer);
        p.rect_filled(outer, 8.0, PANEL);
        let ruler = Rect::from_min_size(outer.min + vec2(0.0, 4.0), vec2(outer.width(), 18.0));
        let flags_h = 20.0;
        let wave_rect = Rect::from_min_max(pos2(outer.left(), ruler.bottom() + flags_h), outer.max - vec2(0.0, 6.0));
        draw_ruler(&p, ruler, ed.view_start, ed.view_len, rate);

        let x_of = |ed: &Editor, f: f64| wave_rect.left() + ((f - ed.view_start) / ed.view_len) as f32 * wave_rect.width();
        let f_of = |ed: &Editor, x: f32| ed.view_start + ((x - wave_rect.left()) / wave_rect.width()) as f64 * ed.view_len;
        let frame_at = |ed: &Editor, x: f32| f_of(ed, x).clamp(0.0, ed.frames as f64) as u64;

        // Which marker (if any) is under the pointer: the line (±8 px) or its flag.
        let flag_top = ruler.bottom();
        let hit_marker = |ed: &Editor, pos: Pos2| -> Option<Drag> {
            if pos.y < flag_top {
                return None;
            }
            let xi = x_of(ed, ed.in_pt as f64);
            let xo = x_of(ed, ed.out_pt as f64);
            let in_flag = pos.y < wave_rect.top() && pos.x >= xi - 4.0 && pos.x <= xi + 40.0;
            let out_flag = pos.y < wave_rect.top() && pos.x >= xo - 40.0 && pos.x <= xo + 4.0;
            let di = (pos.x - xi).abs();
            let dout = (pos.x - xo).abs();
            match (in_flag || di <= 8.0, out_flag || dout <= 8.0) {
                (true, true) => Some(if in_flag && !out_flag || di < dout { Drag::In } else { Drag::Out }),
                (true, false) => Some(Drag::In),
                (false, true) => Some(Drag::Out),
                _ => None,
            }
        };

        // Edges of cuts and of the selection can be dragged too (±6 px).
        let hit_edge = |ed: &Editor, pos: Pos2| -> Option<Drag> {
            if !wave_rect.contains(pos) {
                return None;
            }
            let mut cands: Vec<(f32, Drag)> = Vec::new();
            for (i, &(a, b)) in ed.cuts.iter().enumerate() {
                cands.push(((pos.x - x_of(ed, a as f64)).abs(), Drag::CutEdge(i, false)));
                cands.push(((pos.x - x_of(ed, b as f64)).abs(), Drag::CutEdge(i, true)));
            }
            if let Some((a, b)) = ed.sel {
                cands.push(((pos.x - x_of(ed, a as f64)).abs(), Drag::SelEdge(false)));
                cands.push(((pos.x - x_of(ed, b as f64)).abs(), Drag::SelEdge(true)));
            }
            cands
                .into_iter()
                .filter(|(d, _)| *d <= 6.0)
                .min_by(|x, y| x.0.partial_cmp(&y.0).unwrap())
                .map(|(_, d)| d)
        };

        // Interaction
        let hover = resp.hover_pos();
        if let Some(h) = hover {
            if ed.drag.is_none() && (hit_marker(ed, h).is_some() || hit_edge(ed, h).is_some()) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            } else if ed.drag.is_none() && wave_rect.contains(h) && ui.input(|i| i.modifiers.shift) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
            }
            // Scroll wheel zooms around the mouse; Shift+wheel or horizontal scroll pans.
            let (sd, zoom, shift) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.shift));
            let (zoom_d, pan_d) = if shift { (0.0, sd.x + sd.y) } else { (sd.y, sd.x) };
            if zoom_d != 0.0 {
                let f = f_of(ed, h.x);
                ed.zoom((-zoom_d as f64 / 200.0).exp(), f);
            }
            if zoom != 1.0 {
                let f = f_of(ed, h.x);
                ed.zoom(1.0 / zoom as f64, f);
            }
            if pan_d != 0.0 {
                ed.view_start -= pan_d as f64 / wave_rect.width() as f64 * ed.view_len;
                ed.clamp_view();
            }
        }
        if resp.drag_started() {
            // Use where the button went down, not where the drag was detected.
            let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
            if let Some(pos) = origin {
                // Plain drag moves the playhead; Shift + drag selects a piece.
                let shift = ui.input(|i| i.modifiers.shift);
                ed.drag = Some(if resp.drag_started_by(egui::PointerButton::Primary) {
                    hit_marker(ed, pos).or_else(|| hit_edge(ed, pos)).unwrap_or(if shift {
                        Drag::Select(frame_at(ed, pos.x))
                    } else {
                        Drag::Playhead
                    })
                } else {
                    Drag::Pan(f_of(ed, pos.x))
                });
                if let Some(Drag::CutEdge(i, _)) = ed.drag {
                    ed.undo.push(ed.cuts.clone());
                    ed.sel = None;
                    ed.sel_cut = Some(i);
                }
            }
        }
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let f = frame_at(ed, pos.x);
                match ed.drag {
                    Some(Drag::In) => ed.in_pt = f.min(ed.out_pt.saturating_sub(1)),
                    Some(Drag::Out) => ed.out_pt = f.max(ed.in_pt + 1).min(ed.frames),
                    Some(Drag::Select(anchor)) => {
                        ed.sel = Some((anchor.min(f), anchor.max(f)));
                        ed.sel_cut = None;
                    }
                    Some(Drag::SelEdge(end)) => {
                        if let Some((a, b)) = ed.sel {
                            ed.sel = Some(if end { (a, f.max(a + 1)) } else { (f.min(b.saturating_sub(1)), b) });
                        }
                    }
                    Some(Drag::CutEdge(i, end)) => {
                        // stay clear of the neighbouring cuts, keep at least 10 ms
                        let min_len = (rate as u64 / 100).max(1);
                        let n = ed.cuts.len();
                        if i < n {
                            let lo = if i > 0 { ed.cuts[i - 1].1 } else { 0 };
                            let hi = if i + 1 < n { ed.cuts[i + 1].0 } else { ed.frames };
                            let (a, b) = ed.cuts[i];
                            if end {
                                ed.cuts[i].1 = f.min(hi).max(a + min_len);
                            } else {
                                ed.cuts[i].0 = f.max(lo).min(b.saturating_sub(min_len));
                            }
                        }
                    }
                    Some(Drag::Playhead) => {
                        ed.playhead = f;
                    }
                    Some(Drag::Pan(anchor)) => {
                        let rel = (pos.x - wave_rect.left()) as f64 / wave_rect.width() as f64;
                        ed.view_start = anchor - rel * ed.view_len;
                        ed.clamp_view();
                    }
                    None => {}
                }
                // keep scrolling when dragging a selection or edge past the side
                if matches!(ed.drag, Some(Drag::Select(_) | Drag::SelEdge(_) | Drag::CutEdge(..) | Drag::Playhead)) {
                    if pos.x > wave_rect.right() - 4.0 {
                        ed.view_start += ed.view_len * 0.01;
                        ed.clamp_view();
                    } else if pos.x < wave_rect.left() + 4.0 {
                        ed.view_start -= ed.view_len * 0.01;
                        ed.clamp_view();
                    }
                }
            }
        }
        if matches!(ed.drag, Some(Drag::In | Drag::Out | Drag::CutEdge(..) | Drag::SelEdge(_))) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.drag_stopped() {
            if ed.drag == Some(Drag::Playhead) {
                ed.sel_cut = ed.cut_at(ed.playhead);
                if playing {
                    play_req = Some(ed.play_from(ed.playhead));
                }
            }
            if let (Some(Drag::Select(_)), Some((a, b))) = (ed.drag, ed.sel) {
                // a tiny drag is really a click
                if b - a < (rate as u64 / 50).max(1) {
                    ed.sel = None;
                }
            }
            if let Some(Drag::CutEdge(..)) = ed.drag {
                // nothing changed: don't leave an empty undo step
                if ed.undo.last() == Some(&ed.cuts) {
                    ed.undo.pop();
                }
            }
            ed.drag = None;
        }
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let f = frame_at(ed, pos.x);
                ed.sel = None;
                ed.sel_cut = ed.cut_at(f);
                ed.playhead = f;
                if playing {
                    play_req = Some(ed.play_from(f));
                }
            }
        }

        // Selection background (IN..UIT)
        let xin = x_of(ed, ed.in_pt as f64);
        let xout = x_of(ed, ed.out_pt as f64);
        let sel = Rect::from_min_max(pos2(xin.max(wave_rect.left()), wave_rect.top()), pos2(xout.min(wave_rect.right()), wave_rect.bottom()));
        if sel.width() > 0.0 {
            p.rect_filled(sel, 0.0, Color32::from_rgba_unmultiplied(0x5f, 0xb3, 0xff, 22));
        }

        // Wave
        let (inp, outp) = (ed.in_pt as f64, ed.out_pt as f64);
        let cuts = ed.cuts.clone();
        let color_for = move |f: f64| {
            if cuts.iter().any(|&(a, b)| f >= a as f64 && f < b as f64) {
                WAVE_CUT
            } else if f >= inp && f < outp {
                WAVE
            } else {
                WAVE_DIM
            }
        };
        let fpp = ed.view_len / wave_rect.width() as f64;
        let ch = ed.fmt.channels as usize;
        if fpp < ed.peaks.bin_frames as f64 {
            let (vs, vl) = (ed.view_start, ed.view_len);
            if let Some((s0, samples)) = ed.samples_for_view() {
                draw_wave(&p, wave_rect, &WaveSource::Samples(samples, s0, ch), ch, vs, vl, &color_for);
            }
        } else {
            draw_wave(&p, wave_rect, &WaveSource::Peaks(&ed.peaks), ch, ed.view_start, ed.view_len, &color_for);
        }

        // Cut-out parts
        for (i, &(a, b)) in ed.cuts.iter().enumerate() {
            let xa = x_of(ed, a as f64).max(wave_rect.left());
            let xb = x_of(ed, b as f64).min(wave_rect.right());
            if xb <= xa {
                continue;
            }
            let r = Rect::from_min_max(pos2(xa, wave_rect.top()), pos2(xb, wave_rect.bottom()));
            let selected = ed.sel_cut == Some(i);
            p.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0xe5, 0x48, 0x4d, if selected { 60 } else { 34 }));
            let edge = Stroke::new(if selected { 2.0 } else { 1.0 }, RED);
            p.line_segment([pos2(xa, r.top()), pos2(xa, r.bottom())], edge);
            p.line_segment([pos2(xb, r.top()), pos2(xb, r.bottom())], edge);
            // grab handles
            for (x, visible) in [(xa, x_of(ed, a as f64) >= wave_rect.left()), (xb, x_of(ed, b as f64) <= wave_rect.right())] {
                if visible {
                    for y in [r.top(), r.bottom() - 18.0] {
                        p.rect_filled(Rect::from_min_size(pos2(x - 4.0, y), vec2(8.0, 18.0)), 2.0, RED);
                    }
                }
            }
            if r.width() > 80.0 {
                p.text(pos2(r.center().x, r.top() + 4.0), Align2::CENTER_TOP, "weggeknipt", FontId::proportional(11.5), TEXT);
            }
        }

        // Current selection (to be cut)
        if let Some((a, b)) = ed.sel {
            let xa = x_of(ed, a as f64).max(wave_rect.left());
            let xb = x_of(ed, b as f64).min(wave_rect.right());
            if xb > xa {
                let r = Rect::from_min_max(pos2(xa, wave_rect.top()), pos2(xb.max(xa + 1.0), wave_rect.bottom()));
                p.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40));
                p.rect_stroke(r, 0.0, Stroke::new(1.0, TEXT.gamma_multiply(0.8)), StrokeKind::Inside);
                for (x, f) in [(xa, a), (xb, b)] {
                    let fx = x_of(ed, f as f64);
                    if fx >= wave_rect.left() && fx <= wave_rect.right() {
                        for y in [r.top(), r.bottom() - 18.0] {
                            p.rect_filled(Rect::from_min_size(pos2(x - 4.0, y), vec2(8.0, 18.0)), 2.0, TEXT);
                        }
                    }
                }
            }
        }

        // Markers
        for (f, col, label) in [(ed.in_pt, IN_COL, "IN"), (ed.out_pt, OUT_COL, "UIT")] {
            let x = x_of(ed, f as f64);
            if x >= wave_rect.left() - 1.0 && x <= wave_rect.right() + 1.0 {
                p.line_segment([pos2(x, wave_rect.top() - flags_h + 2.0), pos2(x, wave_rect.bottom())], Stroke::new(2.0, col));
                let flag_w = 34.0;
                let fr = if label == "IN" {
                    Rect::from_min_size(pos2(x, wave_rect.top() - flags_h + 2.0), vec2(flag_w, flags_h - 4.0))
                } else {
                    Rect::from_min_size(pos2(x - flag_w, wave_rect.top() - flags_h + 2.0), vec2(flag_w, flags_h - 4.0))
                };
                p.rect_filled(fr, 3.0, col);
                p.text(fr.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.5), BG);
            }
        }
        // Playhead
        let xp = x_of(ed, ed.playhead as f64);
        if xp >= wave_rect.left() && xp <= wave_rect.right() {
            p.line_segment([pos2(xp, ruler.bottom()), pos2(xp, wave_rect.bottom())], Stroke::new(1.5, PLAYHEAD));
            p.add(egui::Shape::convex_polygon(
                vec![pos2(xp - 6.0, ruler.bottom()), pos2(xp + 6.0, ruler.bottom()), pos2(xp, ruler.bottom() + 8.0)],
                PLAYHEAD,
                Stroke::NONE,
            ));
        }
        if let Some(h) = hover {
            if wave_rect.contains(h) && ed.drag.is_none() {
                let f = frame_at(ed, h.x);
                p.line_segment([pos2(h.x, wave_rect.top()), pos2(h.x, wave_rect.bottom())], Stroke::new(1.0, TEXT.gamma_multiply(0.25)));
                p.text(pos2(h.x + 6.0, wave_rect.bottom() - 4.0), Align2::LEFT_BOTTOM, fmt_time(f, rate, true), FontId::monospace(11.0), TEXT);
            }
        }

        // Overview with visible window
        ui.add_space(6.0);
        let (ov, ov_resp) = ui.allocate_exact_size(vec2(avail.x, 44.0), Sense::click_and_drag());
        let p = ui.painter_at(ov);
        p.rect_filled(ov, 6.0, PANEL);
        let ovw = ov.shrink2(vec2(0.0, 3.0));
        let total = ed.frames.max(1) as f64;
        draw_wave(&p, ovw, &WaveSource::Peaks(&ed.peaks), 1, 0.0, total, &color_for);
        let ox = |f: f64| ovw.left() + (f / total) as f32 * ovw.width();
        let vis = Rect::from_min_max(pos2(ox(ed.view_start), ov.top()), pos2(ox(ed.view_start + ed.view_len).max(ox(ed.view_start) + 3.0), ov.bottom()));
        p.rect_filled(vis, 3.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22));
        p.rect_stroke(vis, 3.0, Stroke::new(1.0, TEXT.gamma_multiply(0.6)), StrokeKind::Inside);
        p.line_segment([pos2(ox(ed.in_pt as f64), ov.top()), pos2(ox(ed.in_pt as f64), ov.bottom())], Stroke::new(2.0, IN_COL));
        p.line_segment([pos2(ox(ed.out_pt as f64), ov.top()), pos2(ox(ed.out_pt as f64), ov.bottom())], Stroke::new(2.0, OUT_COL));
        p.line_segment([pos2(ox(ed.playhead as f64), ov.top()), pos2(ox(ed.playhead as f64), ov.bottom())], Stroke::new(1.0, PLAYHEAD));
        if ov_resp.clicked() || ov_resp.dragged() {
            if let Some(pos) = ov_resp.interact_pointer_pos() {
                let f = ((pos.x - ovw.left()) / ovw.width()) as f64 * total;
                ed.view_start = f - ed.view_len / 2.0;
                ed.clamp_view();
            }
        }
        ov_resp.on_hover_cursor(egui::CursorIcon::PointingHand);

        // Cut bar
        ui.add_space(8.0);
        let cut_target = ed.sel.map(|s| (s, false)).or(ed.sel_cut.and_then(|i| ed.cuts.get(i).map(|&c| (c, true))));
        egui::Frame::NONE
            .fill(if cut_target.is_some() { PANEL2 } else { PANEL })
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), Layout::left_to_right(Align::Center), |ui| {
                    match cut_target {
                        Some(((a, b), is_cut)) => {
                            let what = if is_cut { "Weggeknipt stuk" } else { "Selectie" };
                            ui.label(egui::RichText::new(what).strong().color(if is_cut { RED } else { TEXT }));
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} – {}  ({})",
                                    fmt_time(a, rate, true),
                                    fmt_time(b, rate, true),
                                    fmt_secs(b - a, rate)
                                ))
                                .font(FontId::monospace(14.0))
                                .color(TEXT),
                            );
                            ui.add_space(10.0);
                            if ibutton(ui, Some(Icon::Play), "Beluister de knip", true, None)
                                .on_hover_text("3 seconden ervoor, dan meteen 3 seconden erna — zoals het na het knippen klinkt")
                                .clicked()
                            {
                                let mut v = segs_tail(&ed.keep_segments(0, a), listen);
                                v.extend(segs_head(&ed.keep_segments(b, ed.frames), listen));
                                if let Some(&(s, _)) = v.first() {
                                    ed.playhead = s;
                                }
                                play_req = Some(v);
                            }
                            if is_cut {
                                if ibutton(ui, None, "Terugzetten", true, None).on_hover_text("Dit stuk weer meenemen").clicked() {
                                    if let Some(i) = ed.sel_cut {
                                        ed.restore_cut(i);
                                    }
                                }
                            } else {
                                if ibutton(ui, Some(Icon::Cut), "Knip weg", true, Some(RED.gamma_multiply(0.8)))
                                    .on_hover_text("Sneltoets: Delete")
                                    .clicked()
                                {
                                    ed.add_cut(a, b);
                                    ed.sel = None;
                                }
                                if ibutton(ui, None, "Annuleer", true, None).on_hover_text("Esc").clicked() {
                                    ed.sel = None;
                                }
                            }
                        }
                        None => {
                            paint_icon(ui.painter(), Icon::Cut, ui.cursor().min + vec2(8.0, 16.0), 14.0, DIM);
                            ui.add_space(22.0);
                            ui.label(
                                egui::RichText::new("Stuk weghalen: houd Shift ingedrukt en sleep over het stuk dat weg moet, klik dan op 'Knip weg'. De randen kun je daarna nog verslepen.")
                                    .color(DIM),
                            );
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !ed.cuts.is_empty() || !ed.undo.is_empty() {
                            if ibutton(ui, None, "Ongedaan maken", !ed.undo.is_empty(), None).on_hover_text("Ctrl+Z").clicked() {
                                ed.undo();
                            }
                            let n = ed.cuts.len();
                            if n > 0 {
                                ui.label(
                                    egui::RichText::new(format!("{n} {} weggeknipt", if n == 1 { "stuk" } else { "stukken" })).color(DIM),
                                );
                            }
                        }
                    });
                });
            });

        // Controls: IN on the left, transport in the middle, UIT on the right.
        ui.add_space(10.0);
        let marker_badge = |ui: &mut egui::Ui, label: &str, col: Color32| {
            let (r, _) = ui.allocate_exact_size(vec2(40.0, 26.0), Sense::hover());
            ui.painter().rect_filled(r, 4.0, col);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), BG);
        };
        let kept = export::output_frames(&ed.keep_segments(ed.in_pt, ed.out_pt), CROSSFADE_MS as u64 * rate as u64 / 1000);
        ui.columns(3, |cols| {
            // ---- IN (left) ----
            let ui = &mut cols[0];
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), Layout::left_to_right(Align::Center), |ui| {
                marker_badge(ui, "IN", IN_COL);
                ui.label(egui::RichText::new(fmt_time(ed.in_pt, rate, true)).font(FontId::monospace(18.0)).color(TEXT));
            });
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), Layout::left_to_right(Align::Center), |ui| {
                if ibutton(ui, Some(Icon::ToIn), "Beluister vanaf IN", true, None)
                    .on_hover_text("Speelt af vanaf het IN-punt")
                    .clicked()
                {
                    ed.playhead = ed.in_pt;
                    ed.show_frame(ed.in_pt);
                    play_req = Some(ed.keep_segments(ed.in_pt, ed.out_pt));
                }
                if ibutton(ui, None, "Cursor is IN-punt", true, None)
                    .on_hover_text("Zet het IN-punt op de afspeelpositie (toets I)")
                    .clicked()
                {
                    ed.in_pt = ed.playhead.min(ed.out_pt.saturating_sub(1));
                }
            });

            // ---- Transport (middle) ----
            let ui = &mut cols[1];
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(fmt_time(ed.playhead, rate, true)).font(FontId::monospace(22.0)).color(TEXT));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let row_w = 104.0 + 8.0 + 3.0 * 40.0 + 2.0 * 8.0;
                    ui.add_space(((ui.available_width() - row_w) / 2.0).max(0.0));
                    let (icon, label) = if playing { (Icon::Pause, "Pauze") } else { (Icon::Play, "Afspelen") };
                    if ibutton(ui, Some(icon), label, true, Some(if playing { PANEL2 } else { Color32::from_rgb(0x1f, 0x5f, 0x9e) }))
                        .on_hover_text("Spatiebalk")
                        .clicked()
                    {
                        if playing {
                            stop_req = true;
                        } else {
                            play_req = Some(ed.play_from(ed.playhead));
                        }
                    }
                    if ibutton(ui, Some(Icon::Minus), "", true, None).on_hover_text("Uitzoomen (−)").clicked() {
                        let a = ed.playhead as f64;
                        ed.zoom(2.0, a);
                    }
                    if ibutton(ui, Some(Icon::Plus), "", true, None).on_hover_text("Inzoomen (+ of scrollwiel)").clicked() {
                        let a = ed.playhead as f64;
                        ed.zoom(0.5, a);
                    }
                    if ibutton(ui, Some(Icon::Fit), "", true, None).on_hover_text("Hele opname tonen").clicked() {
                        ed.view_start = 0.0;
                        ed.view_len = ed.frames as f64;
                    }
                });
                ui.add_space(4.0);
                ui.label(egui::RichText::new(format!("Lengte na bewerken: {}", fmt_time(kept, rate, true))).color(DIM));
            });

            // ---- UIT (right, mirrored) ----
            let ui = &mut cols[2];
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), Layout::right_to_left(Align::Center), |ui| {
                marker_badge(ui, "UIT", OUT_COL);
                ui.label(egui::RichText::new(fmt_time(ed.out_pt, rate, true)).font(FontId::monospace(18.0)).color(TEXT));
            });
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), Layout::right_to_left(Align::Center), |ui| {
                if ibutton(ui, Some(Icon::ToOut), "Beluister laatste 3 s", true, None)
                    .on_hover_text("Speelt de laatste 3 seconden voor het UIT-punt, stopt op UIT")
                    .clicked()
                {
                    let v = segs_tail(&ed.keep_segments(ed.in_pt, ed.out_pt), listen);
                    if let Some(&(s, _)) = v.first() {
                        ed.playhead = s;
                        ed.show_frame(s);
                    }
                    play_req = Some(v);
                }
                if ibutton(ui, None, "Cursor is UIT-punt", true, None)
                    .on_hover_text("Zet het UIT-punt op de afspeelpositie (toets O)")
                    .clicked()
                {
                    ed.out_pt = ed.playhead.max(ed.in_pt + 1).min(ed.frames);
                }
            });
        });

        // Export
        ui.add_space(10.0);
        let exporting = self.exporting.is_some();
        egui::Frame::NONE.fill(PANEL).corner_radius(8.0).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Exporteren").strong().color(TEXT));
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Bestandsnaam").color(DIM));
                if ui.add_enabled(!exporting, egui::TextEdit::singleline(&mut ed.export_name).desired_width(380.0)).changed() {
                    ed.name_edited = true;
                }
                ui.label(egui::RichText::new(format!(".{}", self.settings.export_format.ext())).color(DIM));
                ui.add_space(10.0);
                let mut changed = false;
                ui.add_enabled_ui(!exporting, |ui| {
                    changed |= ui.selectable_value(&mut self.settings.export_format, ExportFormat::Flac, "FLAC").changed();
                    changed |= ui.selectable_value(&mut self.settings.export_format, ExportFormat::Wav, "WAV").changed();
                    changed |= ui.selectable_value(&mut self.settings.export_format, ExportFormat::Mp3, "MP3 320k").changed();
                    ui.add_space(10.0);
                    let mut fade = self.settings.fade_ms > 0;
                    if ui
                        .checkbox(&mut fade, "Korte fade (10 ms)")
                        .on_hover_text("Fade-in en fade-out van 10 ms, voorkomt tikjes op de knippunten")
                        .changed()
                    {
                        self.settings.fade_ms = if fade { 10 } else { 0 };
                        changed = true;
                    }
                });
                if changed {
                    self.settings.save();
                }
            });
            // AudioShare: only for a logged-in maker. Programme, date, title and
            // description only matter for the upload, so they're shown only then.
            if self.aus.active_profile().is_some() {
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(2.0);
                let prog = ed.meta.program_id.and_then(|id| self.aus.program(id)).cloned();
                ui.horizontal(|ui| {
                    let mut up = self.settings.auto_upload;
                    if ui.add_enabled(!exporting, egui::Checkbox::new(&mut up, "Na export uploaden naar AudioShare")).changed() {
                        self.settings.auto_upload = up;
                        self.settings.save();
                    }
                    if !self.settings.auto_upload {
                    } else if prog.is_none() {
                        ui.label(egui::RichText::new("Kies hieronder het programma").color(AMBER));
                    } else if let Some(p) = &prog {
                        let secs = kept as f64 / rate as f64;
                        let problems = upload_preview(self.settings.export_format, secs, rate, ed.fmt.channels, &p.quality);
                        if problems.is_empty() {
                            ui.label(egui::RichText::new(format!("Voldoet aan de normen van {}", p.name)).color(GREEN));
                        } else {
                            ui.label(egui::RichText::new(problems.join(" · ")).color(AMBER));
                            let fmt_issue = problems.iter().any(|x| x.contains("Formaat") || x.contains("MB"));
                            if fmt_issue && self.settings.export_format != ExportFormat::Mp3 && ui.button("Gebruik MP3 320k").clicked() {
                                self.settings.export_format = ExportFormat::Mp3;
                                self.settings.save();
                            }
                        }
                    }
                });
                if self.settings.auto_upload {
                    ui.add_space(4.0);
                let mut meta_changed = false;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Programma").color(DIM));
                    let src = self.aus.program_source();
                    meta_changed |= program_picker(ui, &mut ed.meta, &mut self.settings, &mut ed.form, "ed_program", 220.0, &src);
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Uitzenddag").color(DIM));
                    meta_changed |= date_picker(ui, &mut ed.meta.date, "ed_date", 150.0);
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Titel").color(DIM));
                    if ui.add(egui::TextEdit::singleline(&mut ed.meta.title).desired_width(f32::INFINITY).margin(vec2(6.0, 5.0))).changed() {
                        ed.form.title_hint = false;
                        meta_changed = true;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Beschrijving").color(DIM));
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut ed.meta.description)
                                .hint_text(egui::RichText::new("optioneel").color(HINT))
                                .desired_width(f32::INFINITY)
                                .margin(vec2(6.0, 5.0)),
                        )
                        .changed()
                    {
                        ed.form.desc_hint = false;
                        meta_changed = true;
                    }
                });
                if meta_changed {
                    ed.meta.save(&ed.origin);
                    if !ed.name_edited {
                        if let Some(n) = ed.meta.export_name() {
                            ed.export_name = n;
                        }
                    }
                }
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if let Some(x) = &self.exporting {
                    if ibutton(ui, None, "Annuleren", true, None).clicked() {
                        x.cancel.store(true, Ordering::Relaxed);
                    }
                } else if ibutton(ui, None, "Exporteren", true, Some(Color32::from_rgb(0x1f, 0x7a, 0x52))).clicked() {
                    // handled below (needs &mut self)
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("do_export"), true));
                }
                ui.label(
                    egui::RichText::new({
                        let n = ed.cuts.iter().filter(|&&(a, b)| b > ed.in_pt && a < ed.out_pt).count();
                        let cuts = match n {
                            0 => String::new(),
                            1 => ", zonder het weggeknipte stuk".to_string(),
                            n => format!(", zonder de {n} weggeknipte stukken"),
                        };
                        format!("Van IN tot UIT{cuts} ({}), naar dezelfde map als het origineel", fmt_time(kept, rate, false))
                    })
                    .color(DIM),
                );
            });
            if let Some(x) = &self.exporting {
                let pr = x.progress.load(Ordering::Relaxed) as f32 / 1000.0;
                let el = x.started.elapsed().as_secs_f32();
                let eta = if pr > 0.02 { format!(" · nog ca. {:.0} s", el / pr - el) } else { String::new() };
                ui.add(egui::ProgressBar::new(pr).text(format!("{:.0}%{eta}", pr * 100.0)));
            }
            match &self.export_result {
                Some(Ok(p)) => {
                    ui.horizontal(|ui| {
                        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        ui.label(egui::RichText::new(format!("✔ Opgeslagen: {name}")).color(GREEN)).on_hover_text(p.display().to_string());
                        if ui.link("Toon in map").clicked() {
                            show_in_folder(p);
                        }
                    });
                }
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(format!("Export mislukt: {e}")).color(RED));
                }
                None => {}
            }
            if let Some(jid) = &self.last_job {
                let j = self.aus.queue.data.lock().unwrap().jobs.iter().find(|j| &j.id == jid).cloned();
                if let Some(j) = j {
                    use crate::audioshare::queue::JobState as S;
                    let (txt, col) = match j.state {
                        S::Done => (format!("AudioShare: {}", j.message), GREEN),
                        S::Failed => (format!("AudioShare: {}", j.message), RED),
                        S::NeedsConfirm => ("AudioShare: er staat al een aflevering op deze datum — bevestig het vervangen".to_string(), AMBER),
                        S::Cancelled => ("AudioShare: upload geannuleerd".to_string(), DIM),
                        _ => (format!("AudioShare: {}", if j.message.is_empty() { "in de wachtrij" } else { &j.message }), WAVE),
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(txt).color(col));
                        if ui.link("Naar uploads").clicked() {
                            self.mode = Mode::Uploads;
                        }
                    });
                }
            }
        });

        if stop_req {
            self.player.stop();
        }
        if let Some(segs) = play_req {
            let path = self.editor.as_ref().unwrap().path.clone();
            self.player.play(&path, &segs, output_id.as_deref());
            if let Some(err) = self.player.error.clone() {
                self.toast(err, true);
            }
        }
        if ui.ctx().data_mut(|d| d.remove_temp::<bool>(egui::Id::new("do_export"))).unwrap_or(false) {
            self.start_export();
        }
    }

    // -----------------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------------

    /// Messages float above the bottom edge, so nothing shifts when they come and go.
    fn draw_toast(&mut self, ctx: &egui::Context) {
        let Some((msg, at, err)) = self.toast.clone() else { return };
        if at.elapsed() >= Duration::from_secs(if err { 10 } else { 5 }) {
            self.toast = None;
            return;
        }
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("toast"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -18.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let r = egui::Frame::NONE
                    .fill(if err { RED_DARK } else { PANEL2 })
                    .stroke(Stroke::new(1.0, if err { RED } else { LINE }))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(900.0);
                        ui.add(egui::Label::new(egui::RichText::new(&msg).color(TEXT)).wrap());
                    });
                if r.response.interact(Sense::click()).on_hover_text("Klik om te sluiten").clicked() {
                    dismiss = true;
                }
            });
        if dismiss {
            self.toast = None;
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut changed = false;
        egui::Window::new("Instellingen")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Grid::new("settings").num_columns(2).spacing(vec2(14.0, 12.0)).show(ui, |ui| {
                    ui.label("Naam studio");
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut self.settings.studio_name).desired_width(320.0))
                        .on_hover_text("Komt in de FLAC-gegevens, en in de bestandsnaam als er geen programma is gekozen")
                        .changed();
                    ui.end_row();

                    ui.label("Opnamemap");
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&self.settings.record_folder).color(DIM));
                        if ui.button("Kiezen…").clicked() {
                            if let Some(d) = rfd::FileDialog::new().set_directory(&self.settings.record_folder).pick_folder() {
                                self.settings.record_folder = d.to_string_lossy().into_owned();
                                changed = true;
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("Afspelen via");
                    let cur = if self.settings.output_id.is_empty() {
                        "Standaard Windows-apparaat".to_string()
                    } else {
                        self.outputs
                            .iter()
                            .find(|(id, _)| *id == self.settings.output_id)
                            .map(|(_, n)| n.clone())
                            .unwrap_or_else(|| "(niet aangesloten)".into())
                    };
                    egui::ComboBox::from_id_salt("out").width(320.0).selected_text(cur).show_ui(ui, |ui| {
                        if ui.selectable_label(self.settings.output_id.is_empty(), "Standaard Windows-apparaat").clicked() {
                            self.settings.output_id.clear();
                            changed = true;
                        }
                        for (id, name) in &self.outputs {
                            if ui.selectable_label(self.settings.output_id == *id, name).clicked() {
                                self.settings.output_id = id.clone();
                                changed = true;
                            }
                        }
                    });
                    ui.end_row();

                    if !self.aus.configured() {
                    ui.label("Programma's");
                    ui.vertical(|ui| {
                        let mut remove: Option<usize> = None;
                        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                            for (i, p) in self.settings.programs.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.label(p);
                                    if ui.small_button("verwijderen").clicked() {
                                        remove = Some(i);
                                    }
                                });
                            }
                        });
                        if let Some(i) = remove {
                            let p = self.settings.programs.remove(i);
                            self.settings.last_episode.remove(&p);
                            changed = true;
                        }
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.new_program).hint_text(egui::RichText::new("Nieuw programma").color(HINT)).desired_width(220.0));
                            let n = self.new_program.trim().to_string();
                            if ui.add_enabled(!n.is_empty(), egui::Button::new("Toevoegen")).clicked() {
                                if !self.settings.programs.contains(&n) {
                                    self.settings.programs.push(n);
                                    self.settings.programs.sort_by_key(|a| a.to_lowercase());
                                    changed = true;
                                }
                                self.new_program.clear();
                            }
                        });
                    });
                    ui.end_row();
                    }

                    ui.label("Exportformaat");
                    ui.horizontal(|ui| {
                        changed |= ui.selectable_value(&mut self.settings.export_format, ExportFormat::Flac, "FLAC").changed();
                        changed |= ui.selectable_value(&mut self.settings.export_format, ExportFormat::Wav, "WAV").changed();
                        changed |= ui.selectable_value(&mut self.settings.export_format, ExportFormat::Mp3, "MP3 320k").changed();
                    });
                    ui.end_row();
                });
                ui.add_space(8.0);
                ui.separator();
                self.aus.settings_section(ui);
                ui.separator();
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Opnames worden als 24-bit WAV opgeslagen (veilig bij een crash). Bij het exporteren\nmaak je er een getrimde FLAC, WAV of MP3 van; het origineel blijft altijd bewaard.",
                    )
                    .size(12.0)
                    .color(DIM),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("StudioRecorder versie {}", env!("CARGO_PKG_VERSION"))).size(11.0).color(DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        logo(ui, &self.logo, 18.0);
                    });
                });
            });
        if changed {
            self.settings.save();
        }
        if !open {
            self.show_settings = false;
        }
    }

    fn confirm_dialogs(&mut self, ctx: &egui::Context) {
        if self.confirm_stop && !self.mini {
            let m = egui::Modal::new(egui::Id::new("confirm_stop")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(egui::RichText::new("Opname stoppen?").size(18.0).strong());
                ui.add_space(6.0);
                ui.label("De opname wordt afgesloten en geopend om te trimmen.");
                ui.add_space(12.0);
                let mut res = None;
                ui.horizontal(|ui| {
                    if ibutton(ui, Some(Icon::Stop), "Ja, stoppen", true, Some(RED)).clicked() {
                        res = Some(true);
                    }
                    if ibutton(ui, None, "Doorgaan met opnemen", true, None).clicked() {
                        res = Some(false);
                    }
                });
                res
            });
            match m.inner {
                Some(true) => {
                    self.confirm_stop = false;
                    self.stop_recording();
                }
                Some(false) => self.confirm_stop = false,
                None if m.should_close() => self.confirm_stop = false,
                None => {}
            }
        }
        if self.confirm_quit {
            let m = egui::Modal::new(egui::Id::new("confirm_quit")).show(ctx, |ui| {
                ui.set_width(400.0);
                ui.label(egui::RichText::new("Er loopt nog een opname").size(18.0).strong());
                ui.add_space(6.0);
                ui.label("Wil je de opname stoppen en het programma afsluiten?");
                ui.add_space(12.0);
                let mut res = None;
                ui.horizontal(|ui| {
                    if ibutton(ui, None, "Stoppen en afsluiten", true, Some(RED)).clicked() {
                        res = Some(true);
                    }
                    if ibutton(ui, None, "Annuleren", true, None).clicked() {
                        res = Some(false);
                    }
                });
                res
            });
            match m.inner {
                Some(true) => {
                    self.confirm_quit = false;
                    self.stop_recording();
                    self.allow_quit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Some(false) => self.confirm_quit = false,
                None if m.should_close() => self.confirm_quit = false,
                None => {}
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background();

        // Don't quit by accident during a recording.
        if ctx.input(|i| i.viewport().close_requested()) && self.is_recording() && !self.allow_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_quit = true;
        }

        // Drop a WAV or .audioshare file on the window.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            self.open_path(p);
        }
        // Files double-clicked while we were already running.
        let handed: Vec<PathBuf> = self.ipc_rx.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for p in handed {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.open_path(p);
        }

        // AudioShare: background results, sign-out timer, unlinked recorders.
        for (pid, for_editor, info) in self.aus.housekeeping(ctx) {
            if for_editor {
                if let Some(ed) = self.editor.as_mut() {
                    if ed.meta.program_id == Some(pid) && apply_upload_info(&mut ed.meta, &mut ed.form, &info) {
                        ed.meta.save(&ed.origin);
                        if !ed.name_edited {
                            if let Some(n) = ed.meta.export_name() {
                                ed.export_name = n;
                            }
                        }
                    }
                }
            } else if self.meta.program_id == Some(pid) && apply_upload_info(&mut self.meta, &mut self.form, &info) {
                if let (true, Some(p)) = (self.is_recording(), &self.rec_path) {
                    self.meta.save(p);
                }
            }
        }
        for (msg, err) in std::mem::take(&mut self.aus.notices) {
            self.toast(msg, err);
        }
        if let Some(pid) = self.form.want_info.take() {
            self.aus.fetch_info(pid, false);
        }
        if let Some(pid) = self.editor.as_mut().and_then(|e| e.form.want_info.take()) {
            self.aus.fetch_info(pid, true);
        }
        let want_login = std::mem::take(&mut self.form.want_login) | self.editor.as_mut().is_some_and(|e| std::mem::take(&mut e.form.want_login));
        if want_login {
            self.aus.unlock = Some(Default::default());
        }
        let want_refresh = std::mem::take(&mut self.form.want_refresh) | self.editor.as_mut().is_some_and(|e| std::mem::take(&mut e.form.want_refresh));
        if want_refresh {
            self.aus.refresh_programs();
            self.toast("Programmalijst wordt opnieuw opgehaald…", false);
        }
        let title = match (true, self.aus.active_name()) {
            (true, Some(n)) => format!("StudioRecorder v{} — Ingelogd als {n}", env!("CARGO_PKG_VERSION")),
            _ => format!("StudioRecorder v{}", env!("CARGO_PKG_VERSION")),
        };
        if title != self.title_shown {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title_shown = title;
        }

        self.prepare_meta();
        if self.show_settings && !self.aus.admin_unlocked() {
            self.show_settings = false;
        }

        if let Some(e) = &self.engine {
            let peak = e.shared.take_meter();
            self.meter.update(peak);
        }
        if self.free_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            self.free_bytes = free_space(Path::new(&self.settings.record_folder));
            self.free_checked = Some(Instant::now());
        }

        if self.mini {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(BG).inner_margin(egui::Margin::symmetric(12, 8)))
                .show(ctx, |ui| self.mini_screen(ui));
            self.aus.windows(ctx);
            self.confirm_dialogs(ctx);
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }

        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::NONE.fill(PANEL).inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ctx, |ui| self.top_bar(ui));

        if self.mode == Mode::Record {
            let channels = self.engine.as_ref().map(|e| e.shared.channels).unwrap_or(2);
            egui::SidePanel::right("meter")
                .exact_width(130.0)
                .resizable(false)
                .frame(egui::Frame::NONE.fill(PANEL).inner_margin(egui::Margin::symmetric(8, 12)))
                .show(ctx, |ui| {
                    let h = ui.available_height();
                    self.meter.ui(ui, h, channels);
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG).inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| match self.mode {
                Mode::Record => self.record_screen(ui),
                Mode::Edit => self.edit_screen(ui),
                Mode::Uploads => self.aus.uploads_screen(ui),
            });

        self.settings_window(ctx);
        self.aus.windows(ctx);
        self.confirm_dialogs(ctx);
        self.draw_toast(ctx);

        let busy = self.engine.is_some() || self.player.is_playing() || self.loading.is_some() || self.exporting.is_some();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(30));
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.is_recording() {
            self.stop_recording();
        }
        self.player.stop();
        engine::keep_awake(false);
    }
}

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(0x10, 0x12, 0x16);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.weak_bg_fill = PANEL2;
    v.widgets.inactive.bg_fill = PANEL2;
    v.widgets.hovered.weak_bg_fill = PANEL2.linear_multiply(1.3);
    v.selection.bg_fill = Color32::from_rgb(0x1f, 0x5f, 0x9e);
    v.window_corner_radius = CornerRadius::same(10);
    v.override_text_color = Some(TEXT);
    // Always dark, regardless of the Windows light/dark setting.
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Button, FontId::proportional(14.0));
    });
}
