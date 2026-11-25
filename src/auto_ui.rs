//! Venster "Automatische opname" (StudioRecorder 0.9.0).

use crate::auto::{AfterStop, AutoSettings, ProgAuto, ProgRef, Slot, Status};
use crate::export::ExportFormat;
use eframe::egui::{self, Align2, Color32, RichText, vec2};

const DIM: Color32 = Color32::from_rgb(0x9a, 0xa3, 0xb0);
const RED: Color32 = Color32::from_rgb(0xe5, 0x48, 0x4d);
const AMBER: Color32 = Color32::from_rgb(0xf5, 0xa6, 0x23);
const GREEN: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const DAYS: [&str; 7] = ["ma", "di", "wo", "do", "vr", "za", "zo"];

pub struct AutoWindow {
    pub open: bool,
    allowed_text: String,
    loaded: bool,
}

impl Default for AutoWindow {
    fn default() -> Self {
        AutoWindow { open: false, allowed_text: String::new(), loaded: false }
    }
}

fn prog_label(programs: &[(ProgRef, String)], p: &ProgRef) -> String {
    if !p.is_set() {
        return "— geen —".into();
    }
    programs.iter().find(|(r, _)| r == p).map(|(_, n)| n.clone()).unwrap_or_else(|| format!("{} (niet meer beschikbaar)", if p.name.is_empty() { "?" } else { &p.name }))
}

fn prog_combo(ui: &mut egui::Ui, id: impl std::hash::Hash, programs: &[(ProgRef, String)], value: &mut ProgRef, width: f32) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).width(width).selected_text(prog_label(programs, value)).show_ui(ui, |ui| {
        if ui.selectable_label(!value.is_set(), "— geen —").clicked() {
            *value = ProgRef::default();
            changed = true;
        }
        for (r, n) in programs {
            if ui.selectable_label(value == r, n).clicked() {
                *value = r.clone();
                changed = true;
            }
        }
    });
    changed
}

fn format_combo(ui: &mut egui::Ui, id: impl std::hash::Hash, f: &mut ExportFormat) -> bool {
    let label = |f: ExportFormat| match f {
        ExportFormat::Flac => "FLAC",
        ExportFormat::Wav => "WAV",
        ExportFormat::Mp3 => "MP3 320k",
    };
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).width(100.0).selected_text(label(*f)).show_ui(ui, |ui| {
        for x in [ExportFormat::Flac, ExportFormat::Wav, ExportFormat::Mp3] {
            changed |= ui.selectable_value(f, x, label(x)).changed();
        }
    });
    changed
}

/// Statusregel voor het opnamescherm.
pub fn status_line(s: &AutoSettings, st: &Status) -> (String, Color32) {
    if let Some(e) = &st.bind_error {
        return (format!("AUTOMATISCH — {e}"), RED);
    }
    if let Some(r) = &st.running {
        let secs = r.since.elapsed().as_secs();
        return (
            format!("AUTOMATISCH — REC {} · {:02}:{:02}:{:02} · stopt uiterlijk na {} min", r.program_name, secs / 3600, (secs / 60) % 60, secs % 60, s.max_minutes),
            RED,
        );
    }
    let mut t = match st.listening {
        Some(p) => format!("AUTOMATISCH — wacht op START (UDP {p})"),
        None => "AUTOMATISCH — wordt gestart…".into(),
    };
    if st.processing > 0 {
        t.push_str(" · vorige opname wordt verwerkt…");
    } else if let Some((m, _)) = &st.last {
        t.push_str(&format!(" · laatste: {m}"));
    }
    (t, if st.last.as_ref().is_some_and(|l| l.1) { AMBER } else { GREEN })
}

/// Het instellingenvenster. Geeft true terug als er iets is gewijzigd.
pub fn window(
    ctx: &egui::Context,
    w: &mut AutoWindow,
    s: &mut AutoSettings,
    programs: &[(ProgRef, String)],
    studio_linked: bool,
    record_folder: &str,
    may_edit: bool,
) -> bool {
    if !w.open {
        w.loaded = false;
        return false;
    }
    if !w.loaded {
        w.allowed_text = s.allowed.join(", ");
        w.loaded = true;
    }
    let mut changed = false;
    let mut open = true;
    egui::Window::new("Automatische opname")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(760.0)
        .default_height(620.0)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            if !may_edit {
                ui.label(RichText::new("Alleen een studiobeheerder kan deze instellingen wijzigen.").color(AMBER));
                ui.add_space(6.0);
            }
            ui.add_enabled_ui(may_edit, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut s.enabled, RichText::new("Automatische modus aan").strong()).changed();
                    ui.label(RichText::new("Een script start en stopt de opname met START en STOP via UDP.").color(DIM));
                });
                ui.add_space(8.0);

                ui.heading("Verbinding");
                egui::Grid::new("auto_conn").num_columns(2).spacing(vec2(14.0, 8.0)).show(ui, |ui| {
                    ui.label("UDP-poort");
                    changed |= ui.add(egui::DragValue::new(&mut s.port).range(1024..=65535)).changed();
                    ui.end_row();
                    ui.label("Toegestane afzenders");
                    ui.vertical(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut w.allowed_text).desired_width(320.0)).changed() {
                            s.allowed = w.allowed_text.split([',', ';', ' ']).map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect();
                            changed = true;
                        }
                        ui.label(RichText::new("IP-adressen, gescheiden door komma's. 127.0.0.1 = alleen deze pc, * = iedereen.").size(11.5).color(DIM));
                    });
                    ui.end_row();
                    ui.label("Sleutel (optioneel)");
                    ui.vertical(|ui| {
                        changed |= ui.add(egui::TextEdit::singleline(&mut s.key).desired_width(200.0)).changed();
                        ui.label(RichText::new("Ingevuld? Dan moet elk commando key=<sleutel> bevatten, bijv. START key=geheim").size(11.5).color(DIM));
                    });
                    ui.end_row();
                    ui.label("Maximale opnameduur");
                    ui.horizontal(|ui| {
                        changed |= ui.add(egui::DragValue::new(&mut s.max_minutes).range(1..=240).suffix(" min")).changed();
                        ui.label(RichText::new("Komt er geen STOP, dan stopt de opname zelf.").color(DIM));
                    });
                    ui.end_row();
                });
                ui.add_space(10.0);

                ui.heading("Welk programma?");
                ui.label(RichText::new("Bij START kijkt de recorder op volgorde: het bericht (START <programma>), dan het rooster, dan het vaste programma.").color(DIM));
                ui.add_space(4.0);
                changed |= ui.checkbox(&mut s.use_message, "Programma uit het bericht (START DeBlik1)").changed();
                changed |= ui.checkbox(&mut s.use_schedule, "Weekrooster").changed();
                if s.use_schedule {
                    ui.indent("sched", |ui| {
                        let mut remove = None;
                        egui::Grid::new("auto_sched").num_columns(4).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                            for (i, slot) in s.schedule.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    for (d, name) in DAYS.iter().enumerate() {
                                        let day = d as u8 + 1;
                                        let mut on = slot.days.contains(&day);
                                        if ui.toggle_value(&mut on, *name).changed() {
                                            if on { slot.days.push(day) } else { slot.days.retain(|x| *x != day) }
                                            slot.days.sort();
                                            changed = true;
                                        }
                                    }
                                });
                                ui.horizontal(|ui| {
                                    changed |= ui.add(egui::TextEdit::singleline(&mut slot.from).desired_width(54.0)).changed();
                                    ui.label("–");
                                    changed |= ui.add(egui::TextEdit::singleline(&mut slot.to).desired_width(54.0)).changed();
                                });
                                changed |= prog_combo(ui, ("slot", i), programs, &mut slot.program, 220.0);
                                if ui.small_button("verwijderen").clicked() {
                                    remove = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                        if let Some(i) = remove {
                            s.schedule.remove(i);
                            changed = true;
                        }
                        if ui.button("+ Blok toevoegen").clicked() {
                            s.schedule.push(Slot::default());
                            changed = true;
                        }
                        ui.label(RichText::new("Tijden als UU:MM. Een blok over middernacht (23:00–01:00) telt voor de dag waarop het begint. START mag 5 minuten vóór het begin.").size(11.5).color(DIM));
                    });
                }
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut s.use_fixed, "Vast programma").changed();
                    ui.add_enabled_ui(s.use_fixed, |ui| {
                        changed |= prog_combo(ui, "fixed", programs, &mut s.fixed, 260.0);
                    });
                });
                ui.add_space(10.0);

                ui.heading("Na STOP, per programma");
                if !studio_linked {
                    ui.label(RichText::new("Deze pc is niet als studio gekoppeld met AudioShare: alleen opslaan is mogelijk.").color(AMBER));
                }
                if programs.is_empty() {
                    ui.label(RichText::new("Nog geen programma's. Koppel de studio met AudioShare, of voeg programma's toe bij Instellingen.").color(DIM));
                }
                let default_root = std::path::Path::new(record_folder).join("Automatisch");
                for (r, name) in programs {
                    let key = r.key();
                    let mut pa: ProgAuto = s.programs.get(&key).cloned().unwrap_or_default();
                    let before = format!("{:?}", (&pa.after_stop, &pa.folder, pa.save_format, &pa.title_pattern, pa.previous_description, &pa.description));
                    egui::CollapsingHeader::new(RichText::new(name).strong()).id_salt(("prog", &key)).show(ui, |ui| {
                        egui::Grid::new(("pg", &key)).num_columns(2).spacing(vec2(14.0, 6.0)).show(ui, |ui| {
                            ui.label("Na STOP");
                            egui::ComboBox::from_id_salt(("after", &key)).width(180.0).selected_text(pa.after_stop.label()).show_ui(ui, |ui| {
                                for a in [AfterStop::Save, AfterStop::Upload, AfterStop::Both] {
                                    ui.add_enabled_ui(studio_linked || !a.uploads(), |ui| {
                                        ui.selectable_value(&mut pa.after_stop, a, a.label());
                                    });
                                }
                            });
                            ui.end_row();
                            if pa.after_stop.saves() {
                                ui.label("Opslaan in");
                                ui.horizontal(|ui| {
                                    let shown = if pa.folder.trim().is_empty() {
                                        default_root.join(crate::meta::safe_file_part(name)).display().to_string()
                                    } else {
                                        pa.folder.clone()
                                    };
                                    ui.label(RichText::new(shown).color(DIM));
                                    if ui.button("Kiezen…").clicked() {
                                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                            pa.folder = d.to_string_lossy().into_owned();
                                        }
                                    }
                                    if !pa.folder.is_empty() && ui.small_button("standaard").clicked() {
                                        pa.folder.clear();
                                    }
                                });
                                ui.end_row();
                                ui.label("Formaat opslaan");
                                format_combo(ui, ("fmt", &key), &mut pa.save_format);
                                ui.end_row();
                            }
                            if pa.after_stop.uploads() {
                                ui.label("Upload");
                                ui.label(RichText::new("MP3 320k, uitzenddatum = de dag van de opname").color(DIM));
                                ui.end_row();
                            }
                            ui.label("Titel");
                            ui.vertical(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut pa.title_pattern).desired_width(300.0));
                                ui.label(RichText::new("{programma}, {datum} (20 november), {tijd}, {week}").size(11.5).color(DIM));
                            });
                            ui.end_row();
                            ui.label("Omschrijving");
                            ui.vertical(|ui| {
                                if r.id.is_some() {
                                    ui.checkbox(&mut pa.previous_description, "Die van de vorige aflevering (uit AudioShare)");
                                }
                                ui.add(egui::TextEdit::multiline(&mut pa.description).desired_rows(2).desired_width(300.0).hint_text(if pa.previous_description && r.id.is_some() { "Als er geen vorige is" } else { "Vaste tekst" }));
                            });
                            ui.end_row();
                        });
                    });
                    let after = format!("{:?}", (&pa.after_stop, &pa.folder, pa.save_format, &pa.title_pattern, pa.previous_description, &pa.description));
                    if before != after {
                        s.programs.insert(key, pa);
                        changed = true;
                    }
                }
                ui.add_space(10.0);
                ui.separator();
                ui.label(RichText::new("Testen vanuit PowerShell op deze pc:").color(DIM));
                ui.add(
                    egui::TextEdit::multiline(&mut format!(
                        "$u = New-Object System.Net.Sockets.UdpClient\n$b = [Text.Encoding]::UTF8.GetBytes(\"START\"); $u.Send($b, $b.Length, \"127.0.0.1\", {})",
                        s.port
                    ))
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
                );
            });
            });
        });
    if !open {
        w.open = false;
    }
    changed
}
