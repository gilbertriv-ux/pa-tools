//! The menu bar, built from the incumbent's menu catalog so every item has its place. Items
//! that map to a command are live; the rest are shown disabled (and listed in the parity report).

use crate::{MainWindow, SoundApp};
use serde_json::{Value, json};
use soundcraft_engine::catalog;

/// UI-layer commands: (id, label, catalog path or "", shortcut).
pub const UI_COMMANDS: &[(&str, &str, &str, Option<&str>)] = &[
    ("ui.dock", "Arrange Panel", "", None),
    ("ui.dock.reset", "Reset Panel Layout", "Window > Reset Panel Layout", None),
    ("window.mix", "Mix", "Window > Mix", Some("Cmd+=")),
    ("window.edit", "Edit", "Window > Edit", Some("Cmd+=")),
    ("window.toggle_mix_edit", "Toggle Mix/Edit", "", Some("Cmd+=")),
    ("window.mix_separate", "Mix in Separate Window", "", None),
    ("window.transport", "Transport", "Window > Transport", Some("Cmd+1")),
    ("window.big_counter", "Big Counter", "Window > Big Counter", Some("Cmd+3")),
    ("window.memory_locations", "Memory Locations", "Window > Memory Locations", Some("Cmd+5")),
    ("window.undo_history", "Undo History", "Window > Undo History", None),
    ("window.clip_list", "Clip List", "Window > Clip List", None),
    ("window.track_list", "Track List", "View > Other Displays > Track List", None),
    ("window.clip_list_view", "Clip List", "View > Other Displays > Clip List", None),
    ("window.narrow_mix", "Narrow Mix", "View > Narrow Mix", Some("Cmd+Alt+M")),
    ("window.session_info", "Session Info", "", None),
    ("window.audio_health", "Session Audio Health", "", None),
    ("window.about", "About Pa Tools", "", None),
    ("window.close", "Close Window", "Window > Close Window", Some("Cmd+W")),
    ("window.hide_floating", "Hide All Floating Windows", "Window > Hide All Floating Windows", Some("Cmd+Ctrl+W")),
    ("window.midi_editor", "MIDI Editor", "Window > MIDI Editor", None),
    ("window.universe", "Universe", "View > Other Displays > Universe", Some("Alt+7")),
    ("window.video", "Video", "Window > Video", Some("Cmd+9")),
    ("window.transcript", "Transcript", "Window > Transcript", None),
    ("window.video_universe", "Video Universe", "Window > Video Universe", None),
    ("window.automation", "Automation", "Window > Automation", Some("Cmd+4")),
    ("window.color_palette", "Color Palette", "Window > Color Palette", None),
    ("window.disk_usage", "Disk Usage", "Window > Disk Usage", None),
    ("window.system_usage", "System Usage", "Window > System Usage", None),
    ("window.task_manager", "Task Manager", "Window > Task Manager", Some("Alt+'")),
    ("window.metadata", "Metadata Inspector", "Window > Metadata Inspector", None),
    ("window.event_list", "MIDI Event List", "Window > MIDI Event List", Some("Alt+=")),
    ("window.midi_keyboard", "MIDI Keyboard", "Window > MIDI Keyboard", Some("Shift+K")),
    ("window.workspace", "Workspace", "Window > New Workspace > Default", Some("Alt+I")),
    ("window.workspace_presets", "Workspace", "Window > New Workspace > Track Presets", None),
    ("window.workspace_sounds", "Workspace", "Window > New Workspace > Sound Browser", None),
    ("window.workspace_front", "Workspaces to Front", "Window > Workspaces > Bring to Front", Some("Alt+J")),
    ("window.workspace_close", "Close All Workspaces", "Window > Workspaces > Close All Workspaces", None),
    ("window.configurations", "Window Configuration List", "Window > Configurations > Window Configuration List", Some("Cmd+Alt+J")),
    ("window.config_new", "New Configuration", "Window > Configurations > New Configuration...", None),
    ("window.config_update", "Update Active Configuration", "Window > Configurations > Update Active Configuration", None),
    ("window.config_auto_update", "Auto-Update Active Configuration", "Window > Configurations > Auto-Update Active Configuration", None),
    ("window.renderer", "Renderer", "Window > Renderer", None),
    ("window.ui_customization", "UI Customization", "Window > UI Customization", None),
    ("window.arrange_tile", "Tile", "Window > Arrange > Tile", None),
    ("window.arrange_tile_h", "Tile Horizontal", "Window > Arrange > Tile Horizontal", None),
    ("window.arrange_tile_v", "Tile Vertical", "Window > Arrange > Tile Vertical", None),
    ("window.arrange_cascade", "Cascade", "Window > Arrange > Cascade", None),
    ("window.midi_front", "MIDI Editors to Front", "Window > MIDI Editors > Bring to Front", None),
    ("window.midi_back", "MIDI Editors to Back", "Window > MIDI Editors > Send to Back", None),
    ("window.clip_effects", "Clip Effects", "Window > Clip Effects", None),
    ("window.clip_effects_dock", "Clip Effects", "View > Other Displays > Lower Dock > Clip Effects", Some("Alt+6")),
    ("window.search", "Search", "Window > Pa Tools Search", Some("Cmd+Ctrl+S")),
    ("window.score", "Score Editor", "Window > Score Editor", Some("Cmd+Ctrl+=")),
    ("window.beat_detective", "Beat Detective", "", None),
    ("window.tempo_ops", "Tempo Operations", "", None),
    ("window.time_ops", "Time Operations", "", None),
    ("window.midi_ops", "MIDI Operations", "", None),
    ("window.rtp", "MIDI Real-Time Properties", "", None),
    ("window.playback_engine", "Playback Engine", "", None),
    ("window.midi_thru", "MIDI Thru", "Setup > MIDI Thru...", None),
    ("window.channel_routing", "Channel Routing", "Setup > Channel Routing...", None),
    ("ui.audio_device", "Audio Device", "", None),
    ("window.io_setup", "I/O Setup", "", None),
    ("window.shortcuts", "Keyboard Shortcuts", "", None),
    ("view.mix_section", "Mix Window View", "", None),
    ("ui.new_tracks_dialog", "New Tracks…", "", None),
    ("ui.bounce_dialog", "Bounce Mix…", "", None),
    ("ui.set", "Set UI State", "", None),
    ("ui.theme", "Appearance", "", None),
];

/// Extra mappings handled by the UI layer, keyed by the paths the Pa Tools menu shows (see [`RENAMED`]).
/// Menu rendering, the native menu bar and menu-style invocation use these.
pub fn ui_aliases() -> Vec<(&'static str, &'static str)> {
    let mut v: Vec<(&str, &str)> = UI_COMMANDS.iter().filter(|c| !c.2.is_empty()).map(|c| (c.2, c.0)).collect();
    v.extend(mix_view_aliases().iter().copied());
    v.extend(AUDIOSUITE.iter().map(|(p, _)| (*p, "audiosuite.process")));
    v.extend(MENU_WINDOWS.iter().copied());
    v
}

/// The same mappings keyed by the incumbent's catalog paths. Only the parity accounting
/// (`engine.parity`, `ui.menu.list`) uses these: the catalog stays factual, so the measure compares
/// the menu tree of the target with what we implement, whatever we call the items.
pub fn parity_aliases() -> Vec<(&'static str, &'static str)> {
    let mut v: Vec<(&str, &str)> = UI_COMMANDS.iter().filter(|c| !c.2.is_empty()).map(|c| (catalog_path(c.2), c.0)).collect();
    v.extend(mix_view_aliases().iter().copied());
    v.extend(PARITY_ALIASES.iter().copied());
    v.extend(MENU_WINDOWS.iter().copied());
    v
}

/// The path the Pa Tools menu shows for a catalog path (the same path when it is not renamed).
pub fn display_path(catalog_path: &str) -> &str {
    RENAMED.iter().find(|(c, _)| *c == catalog_path).map_or(catalog_path, |(_, d)| *d)
}

/// The catalog path behind a path the Pa Tools menu shows (the same path when it is not renamed).
pub fn catalog_path(menu_path: &str) -> &str {
    RENAMED.iter().find(|(_, d)| *d == menu_path).map_or(menu_path, |(c, _)| *c)
}

/// Names of other companies' products and trademarks. A catalog leaf that carries one and that no
/// command implements is left out of the menu bar; the catalog and the parity report keep it.
const THIRD_PARTY_MARKS: &[&str] = &[
    "Dolby",
    "Atmos",
    "Melodyne",
    "Splice",
    "SoundFlow",
    "HEAT",
    "Sketch",
    "Media Composer",
    "Avid",
    "Pro Tools",
    "Sibelius",
    "Ableton",
    "Soundbase",
    "VENUE",
];

/// Whether a menu label carries another company's product name or trademark (`ARA` counts as a whole word).
pub fn is_third_party_mark(label: &str) -> bool {
    THIRD_PARTY_MARKS.iter().any(|m| label.contains(m)) || label.split(|c: char| !c.is_alphanumeric()).any(|w| w == "ARA")
}

/// Catalog leaves the menu bar leaves out: unimplemented and named after someone else's product.
/// Decided once, from static data.
fn hidden_leaves() -> &'static std::collections::HashSet<&'static str> {
    static HIDDEN: std::sync::OnceLock<std::collections::HashSet<&'static str>> = std::sync::OnceLock::new();
    HIDDEN.get_or_init(|| {
        let aliases = parity_aliases();
        catalog::catalog()
            .into_iter()
            .filter(|line| {
                let shown = display_path(line);
                is_third_party_mark(shown.rsplit(" > ").next().unwrap_or(shown)) && catalog::implemented_by(line, &aliases).is_none()
            })
            .collect()
    })
}

/// Mix-view menu paths, allocated once. Rebuilding them on every menu draw leaked a copy per frame.
fn mix_view_aliases() -> &'static [(&'static str, &'static str)] {
    static ALIASES: std::sync::OnceLock<Vec<(&'static str, &'static str)>> = std::sync::OnceLock::new();
    ALIASES.get_or_init(|| {
        [
            "Inserts A-E",
            "Inserts F-J",
            "Sends A-E",
            "Sends F-J",
            "I/O",
            "Track Color",
            "Comments",
            "EQ Curve",
            "Meters and Faders",
            "All",
            "Minimal",
            "Mic Preamps",
            "Instruments",
            "Object",
        ]
        .into_iter()
        .map(|sec| {
            let path: &'static str = Box::leak(format!("View > Mix Window Views > {sec}").into_boxed_str());
            (path, "view.mix_section")
        })
        .collect()
    })
}

/// AudioSuite menu entries → our processes (functional equivalents, our own plugins, our own names).
/// Keyed by the path the Pa Tools menu shows; [`RENAMED`] and [`PARITY_ALIASES`] hold the incumbent's
/// catalog paths for the same entries.
pub const AUDIOSUITE: &[(&str, &str)] = &[
    ("AudioSuite > EQ > Channel Strip", "channel_strip"),
    ("AudioSuite > EQ > Pa EQ 1-Band", "eq_1band"),
    ("AudioSuite > EQ > Pa EQ 7-Band", "eq_7band"),
    ("AudioSuite > Dynamics > Pa Comp 76", "compressor"),
    ("AudioSuite > Dynamics > Channel Strip", "channel_strip"),
    ("AudioSuite > Dynamics > Pa Dynamics Compressor/Limiter", "compressor"),
    ("AudioSuite > Dynamics > Pa De-Esser", "de_esser"),
    ("AudioSuite > Dynamics > Pa Expander/Gate", "expander_gate"),
    ("AudioSuite > Dynamics > Pa Maximizer", "maximizer"),
    ("AudioSuite > Pitch Shift > Pa Pitch", "pitch_shift"),
    ("AudioSuite > Pitch Shift > Pa Pitch Shift (classic)", "pitch_shift"),
    ("AudioSuite > Pitch Shift > Time Shift", "time_stretch"),
    ("AudioSuite > Pitch Shift > Pa Varispeed", "varispeed"),
    ("AudioSuite > Reverb > Pa Reverb", "room_reverb"),
    ("AudioSuite > Delay > Pa Mod Delay", "mod_delay"),
    ("AudioSuite > Modulation > Pa Flanger FX", "flanger"),
    ("AudioSuite > Harmonic > Pa Amp Drive", "saturator"),
    ("AudioSuite > Harmonic > Lo-Fi", "lofi"),
    ("AudioSuite > Harmonic > Pa Rectifier", "rectifier"),
    ("AudioSuite > Harmonic > Pa Saturator", "saturator"),
    ("AudioSuite > Other > DC Offset Removal", "dc_offset_removal"),
    ("AudioSuite > Other > Duplicate", "duplicate"),
    ("AudioSuite > Other > Gain", "gain"),
    ("AudioSuite > Other > Invert", "invert"),
    ("AudioSuite > Other > Normalize", "normalize"),
    ("AudioSuite > Other > Reverse", "reverse"),
    ("AudioSuite > Other > Signal Generator", "signal_generator"),
    ("AudioSuite > Other > Time Compression Expansion", "time_stretch"),
];

/// Menu entries the incumbent's catalog names with someone else's product or trademark, which the Pa
/// Tools menu shows under its own name: (catalog path, Pa Tools menu path). The catalog itself
/// (`crates/engine/catalog/menus.txt`) is factual data about the target and stays untouched, so the
/// parity measure still compares like with like; [`display_path`] and [`catalog_path`] translate.
pub const RENAMED: &[(&str, &str)] = &[
    ("AudioSuite > EQ > EQ3 1-Band", "AudioSuite > EQ > Pa EQ 1-Band"),
    ("AudioSuite > EQ > EQ3 7-Band", "AudioSuite > EQ > Pa EQ 7-Band"),
    ("AudioSuite > Dynamics > BF-76", "AudioSuite > Dynamics > Pa Comp 76"),
    ("AudioSuite > Dynamics > Dyn3 Compressor/Limiter", "AudioSuite > Dynamics > Pa Dynamics Compressor/Limiter"),
    ("AudioSuite > Dynamics > Dyn3 De-Esser", "AudioSuite > Dynamics > Pa De-Esser"),
    ("AudioSuite > Dynamics > Dyn3 Expander/Gate", "AudioSuite > Dynamics > Pa Expander/Gate"),
    ("AudioSuite > Dynamics > Maxim", "AudioSuite > Dynamics > Pa Maximizer"),
    ("AudioSuite > Pitch Shift > Pitch II", "AudioSuite > Pitch Shift > Pa Pitch"),
    ("AudioSuite > Pitch Shift > Pitch Shift Legacy", "AudioSuite > Pitch Shift > Pa Pitch Shift (classic)"),
    ("AudioSuite > Pitch Shift > Vari-Fi", "AudioSuite > Pitch Shift > Pa Varispeed"),
    ("AudioSuite > Reverb > D-Verb", "AudioSuite > Reverb > Pa Reverb"),
    ("AudioSuite > Delay > Mod Delay III", "AudioSuite > Delay > Pa Mod Delay"),
    ("AudioSuite > Modulation > Sci-Fi", "AudioSuite > Modulation > Pa Flanger FX"),
    ("AudioSuite > Harmonic > Eleven Lite", "AudioSuite > Harmonic > Pa Amp Drive"),
    ("AudioSuite > Harmonic > Recti-Fi", "AudioSuite > Harmonic > Pa Rectifier"),
    ("AudioSuite > Harmonic > SansAmp PSA-1", "AudioSuite > Harmonic > Pa Saturator"),
    ("File > Export > Sibelius...", "File > Export > Notation Program (MusicXML)..."),
    ("File > Send To... > Sibelius", "File > Send To... > Notation Program"),
    ("View > Transport > Ableton Link", "View > Transport > Link Sync"),
    ("View > Clip > ARA Note Overlay", "View > Clip > Note Overlay"),
    ("Window > New Workspace > Soundbase", "Window > New Workspace > Sound Browser"),
    ("Window > Pro Tools Search", "Window > Pa Tools Search"),
];

/// The AudioSuite entries under their catalog paths, for the parity accounting only. The menu itself
/// uses [`AUDIOSUITE`]; counting these keeps "implemented / 512" honest after the renames.
pub const PARITY_ALIASES: &[(&str, &str)] = &[
    ("AudioSuite > EQ > Channel Strip", "audiosuite.process"),
    ("AudioSuite > EQ > EQ3 1-Band", "audiosuite.process"),
    ("AudioSuite > EQ > EQ3 7-Band", "audiosuite.process"),
    ("AudioSuite > Dynamics > BF-76", "audiosuite.process"),
    ("AudioSuite > Dynamics > Channel Strip", "audiosuite.process"),
    ("AudioSuite > Dynamics > Dyn3 Compressor/Limiter", "audiosuite.process"),
    ("AudioSuite > Dynamics > Dyn3 De-Esser", "audiosuite.process"),
    ("AudioSuite > Dynamics > Dyn3 Expander/Gate", "audiosuite.process"),
    ("AudioSuite > Dynamics > Maxim", "audiosuite.process"),
    ("AudioSuite > Pitch Shift > Pitch II", "audiosuite.process"),
    ("AudioSuite > Pitch Shift > Pitch Shift Legacy", "audiosuite.process"),
    ("AudioSuite > Pitch Shift > Time Shift", "audiosuite.process"),
    ("AudioSuite > Pitch Shift > Vari-Fi", "audiosuite.process"),
    ("AudioSuite > Reverb > D-Verb", "audiosuite.process"),
    ("AudioSuite > Delay > Mod Delay III", "audiosuite.process"),
    ("AudioSuite > Modulation > Sci-Fi", "audiosuite.process"),
    ("AudioSuite > Harmonic > Eleven Lite", "audiosuite.process"),
    ("AudioSuite > Harmonic > Lo-Fi", "audiosuite.process"),
    ("AudioSuite > Harmonic > Recti-Fi", "audiosuite.process"),
    ("AudioSuite > Harmonic > SansAmp PSA-1", "audiosuite.process"),
    ("AudioSuite > Other > DC Offset Removal", "audiosuite.process"),
    ("AudioSuite > Other > Duplicate", "audiosuite.process"),
    ("AudioSuite > Other > Gain", "audiosuite.process"),
    ("AudioSuite > Other > Invert", "audiosuite.process"),
    ("AudioSuite > Other > Normalize", "audiosuite.process"),
    ("AudioSuite > Other > Reverse", "audiosuite.process"),
    ("AudioSuite > Other > Signal Generator", "audiosuite.process"),
    ("AudioSuite > Other > Time Compression Expansion", "audiosuite.process"),
];

/// Commands that open a dialog when invoked from a menu (programmatic calls never do).
const DIALOG_COMMANDS: &[&str] = &[
    "clip.clear",
    "track.new",
    "file.bounce_mix",
    "session.new",
    "file.import_audio",
    "file.import_midi",
    "session.open",
    "session.save",
    "session.save_as",
    "session.save_copy",
    "track.group",
    "edit.repeat",
    "edit.shift",
    "edit.fades_create",
    "setup.session",
    "event.quantize",
    "event.transpose",
    "edit.strip_silence",
    "clip.rename",
];

/// Shared menu hierarchy for egui and native desktop hosts.
pub struct MenuNode {
    pub label: String,
    pub path: String,
    pub children: Vec<MenuNode>,
}

fn insert_path(roots: &mut Vec<MenuNode>, line: &str) {
    let parts: Vec<&str> = line.split(" > ").collect();
    let mut level = roots;
    let mut path = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            path.push_str(" > ");
        }
        path.push_str(p);
        let idx = match level.iter().position(|n| n.label == *p) {
            Some(i) => i,
            None => {
                level.push(MenuNode { label: p.to_string(), path: path.clone(), children: Vec::new() });
                level.len() - 1
            }
        };
        let Some(node) = level.get_mut(idx) else { break };
        level = &mut node.children;
    }
}

pub fn tree() -> Vec<MenuNode> {
    let mut roots: Vec<MenuNode> = Vec::new();
    let hidden = hidden_leaves();
    for line in catalog::catalog() {
        if !hidden.contains(line) {
            insert_path(&mut roots, display_path(line));
        }
    }
    insert_path(&mut roots, "Window > Reset Panel Layout");
    // Not in the observed catalog either; Pro Tools lists it right under Identify Sync Point.
    if let Some(clip) = roots.iter_mut().find(|n| n.label == "Clip") {
        let at = clip.children.iter().position(|n| n.label == "Identify Sync Point").map_or(clip.children.len(), |i| i + 1);
        clip.children.insert(at, MenuNode { label: "Remove Sync Point".into(), path: "Clip > Remove Sync Point".into(), children: Vec::new() });
    }
    // Not in the incumbent catalog, but the only UI path to an empty MIDI clip (#282).
    // It creates a clip rather than acting on the selected ones, so it leads the Clip menu.
    insert_path(&mut roots, "Clip > New MIDI Clip");
    if let Some(clip) = roots.iter_mut().find(|n| n.label == "Clip") {
        clip.children.rotate_right(1);
    }
    for (path, _) in MENU_WINDOWS {
        insert_path(&mut roots, path);
    }
    roots
}

pub fn menu_bar(app: &mut SoundApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::current();
    egui::Panel::top("menu_bar").exact_size(24.0).frame(egui::Frame::NONE.fill(t.menu_bar).inner_margin(egui::Margin::symmetric(8, 2))).show(
        ui,
        |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(soundcraft_brand::APP_NAME, |ui| {
                    if ui.button(format!("About {}", soundcraft_brand::APP_NAME)).clicked() {
                        app.ui.show_about = true;
                    }
                    if ui.button("Session Info").clicked() {
                        app.ui.show_session_info = true;
                    }
                    if ui.button("Session Audio Health").clicked() {
                        let _ = app.run("window.audio_health", json!({"value": true}));
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        let _ = crate::dialogs::run_or_ask(app, "app.quit", json!({}));
                    }
                });
                let extra = ui_aliases();
                for root in tree() {
                    ui.menu_button(&root.label, |ui| {
                        for c in &root.children {
                            menu_node(app, ui, c, &extra);
                            if c.path == "File > Open" {
                                crate::recent::menu(app, ui);
                            }
                            if c.path == "Track > New..." {
                                instrument_menu(app, ui);
                            }
                        }
                        if root.label == "AudioSuite" {
                            ui.separator();
                            ui.label(egui::RichText::new("Processes selected clips offline").small().color(t.text_dim));
                        }
                        if root.label == "Window" {
                            ui.separator();
                            let mut separate = app.ui.mix_separate;
                            if ui
                                .checkbox(&mut separate, "Mix in Separate Window")
                                .on_hover_text("Show Edit and Mix at once, e.g. one per monitor")
                                .changed()
                            {
                                let _ = run_ui_command(app, "window.mix_separate", &json!({"value": separate}));
                                ui.close();
                            }
                        }
                    });
                }
                let title = format!(
                    "{}{}  —  {}",
                    app.engine.session().name,
                    if app.engine.is_dirty() { " *" } else { "" },
                    if app.ui.window == MainWindow::Edit || app.ui.mix_separate { "Edit" } else { "Mix" }
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(title).color(t.text_dim));
                });
            });
        },
    );
}

/// Track › Instrument: the instrument picker for the selected instrument track (#374).
fn instrument_menu(app: &mut SoundApp, ui: &mut egui::Ui) {
    match instrument_track(app.engine.session()) {
        Some(id) => {
            ui.menu_button("Instrument", |ui| crate::mix_window::instrument_menu(app, ui, id));
        }
        None => {
            ui.add_enabled(false, egui::Button::new("Instrument")).on_disabled_hover_text("Select an instrument track");
        }
    }
}

/// The first selected instrument track, in track order.
fn instrument_track(s: &soundcraft_model::Session) -> Option<soundcraft_model::TrackId> {
    s.tracks.iter().find(|t| t.kind == soundcraft_model::TrackKind::Instrument && s.edit.selected_tracks.contains(&t.id)).map(|t| t.id)
}

fn menu_node(app: &mut SoundApp, ui: &mut egui::Ui, n: &MenuNode, extra: &[(&str, &str)]) {
    if !n.children.is_empty() {
        ui.menu_button(&n.label, |ui| {
            for c in &n.children {
                menu_node(app, ui, c, extra);
            }
        });
        return;
    }
    let id = command_for_path(&n.path, extra);
    let (enabled, checked) = item_state(app, &n.path, id.as_deref());
    let label = live_label(app, id.as_deref()).unwrap_or_else(|| n.label.clone());
    let label = if checked { format!("✔ {label}") } else { label };
    let shortcut = id.as_deref().and_then(shortcut_for).unwrap_or("");
    let btn = egui::Button::new(label).shortcut_text(crate::shortcuts::shortcut_label(shortcut, ui.ctx().os().is_mac()));
    if ui.add_enabled(enabled, btn).clicked()
        && let Some(id) = id
    {
        invoke_menu(app, &id, &n.path);
        ui.close();
    }
}

/// Resolve the command behind a menu leaf, including UI dialog overrides.
pub fn command_for_path(path: &str, extra: &[(&str, &str)]) -> Option<String> {
    catalog::implemented_by(path, extra).or_else(|| MENU_WINDOWS.iter().find(|(p, _)| *p == path).map(|(_, w)| w.to_string()))
}

/// Enabled and checked state shared by all menu presentations.
pub fn item_state(app: &SoundApp, path: &str, id: Option<&str>) -> (bool, bool) {
    let enabled = match id {
        Some(i)
            if i.starts_with("window.") || i.starts_with("view.mix_section") || matches!(i, "audiosuite.process" | "ui.dock" | "ui.dock.reset") =>
        {
            true
        }
        Some(i) => soundcraft_engine::find_command(i).is_some_and(|c| (c.enabled)(&app.engine).is_ok() && !app.engine.blocked_while_recording(i)),
        None => false,
    };
    (enabled, checked_state(app, path, id))
}

fn checked_state(app: &SoundApp, path: &str, id: Option<&str>) -> bool {
    let e = &app.engine.session().edit;
    match id {
        Some("options.loop_playback") => e.loop_playback,
        Some("options.loop_record") => e.loop_record,
        Some("options.quickpunch") => e.quickpunch,
        Some("options.pre_post_roll") => e.pre_post_roll,
        Some("options.link_timeline_edit") => e.link_timeline_edit,
        Some("options.link_track_edit") => e.link_track_edit,
        Some("options.insertion_follows_playback") => e.insertion_follows_playback,
        Some("options.tab_to_transient") => e.tab_to_transient,
        Some("options.automation_follows_edit") => e.automation_follows_edit,
        Some("options.markers_follow_edit") => e.markers_follow_edit,
        Some("options.click") => e.click,
        Some("options.delay_compensation") => e.delay_compensation,
        Some("options.pre_fader_metering") => e.pre_fader_metering,
        Some("view.ruler") => path.rsplit(" > ").next().map(ruler_id).is_some_and(|r| e.rulers.iter().any(|x| x == r)),
        Some("view.main_counter") => path.rsplit(" > ").next().map(ruler_id).is_some_and(|r| e.main_counter.id() == r),
        Some("window.mix") => app.ui.window == MainWindow::Mix,
        Some("window.edit") => app.ui.window == MainWindow::Edit,
        Some("window.track_list") => app.ui.show_tracks_list,
        Some("window.clip_list_view") | Some("window.clip_list") => app.ui.show_clip_list,
        Some("window.narrow_mix") => app.ui.narrow_mix,
        Some("window.renderer") => app.extra.show_renderer,
        Some("window.transcript") => app.ui.show_transcript,
        Some("window.ui_customization") => app.extra.show_ui_customization,
        Some("window.config_auto_update") => app.extra.auto_update_config,
        Some("view.mix_section") => path.rsplit(" > ").next().map(mix_section_id).is_some_and(|s| app.ui.mix_views.iter().any(|v| v == s)),
        _ => false,
    }
}

fn ruler_id(label: &str) -> &'static str {
    match label {
        "Bars|Beats" => "bars_beats",
        "Min:Secs" => "min_secs",
        "Timecode" => "timecode",
        "Feet+Frames" => "feet_frames",
        "Samples" => "samples",
        "Tempo" => "tempo",
        "Meter" => "meter",
        "Markers" => "markers",
        "Key Signature" => "key",
        "Chord Symbols" => "chords",
        _ => "",
    }
}

fn mix_section_id(label: &str) -> &'static str {
    match label {
        "Inserts A-E" => "inserts_ae",
        "Inserts F-J" => "inserts_fj",
        "Sends A-E" => "sends_ae",
        "Sends F-J" => "sends_fj",
        "I/O" => "io",
        "Track Color" => "color",
        "Comments" => "comments",
        "EQ Curve" => "eq_curve",
        "Meters and Faders" => "meters",
        "All" => "all",
        "Minimal" => "minimal",
        "Mic Preamps" => "mic_preamps",
        "Instruments" => "instruments",
        "Object" => "object",
        _ => "",
    }
}

/// Parameters implied by the menu item a command was reached through.
fn params_for(path: &str, id: &str) -> Value {
    let leaf = path.rsplit(" > ").next().unwrap_or("");
    match id {
        "view.ruler" => json!({"ruler": ruler_id(leaf)}),
        "view.main_counter" => json!({"format": ruler_id(leaf)}),
        "options.scrolling" => {
            json!({"mode": match leaf { "No Scrolling" => "none", "After Playback" => "after_playback", "Continuous" => "continuous", "Center Playhead" => "center", _ => "page" }})
        }
        "options.solo_mode" => {
            json!({"mode": if leaf.starts_with("AFL") { "afl" } else if leaf.starts_with("PFL") { "pfl" } else { "sip" }, "xor": leaf.starts_with("X-OR")})
        }
        "clip.rating" => json!({"rating": leaf.parse::<u8>().unwrap_or(0)}),
        "track.change_width" => json!({"format": leaf}),
        "view.mix_section" => json!({"section": mix_section_id(leaf)}),
        "audiosuite.process" => {
            let shown = display_path(path);
            json!({"process": AUDIOSUITE.iter().find(|(p, _)| *p == shown).map_or("", |(_, x)| *x)})
        }
        _ => json!({}),
    }
}

/// Menu items that open a Pa Tools window when clicked (programmatic calls still run the command).
const MENU_WINDOWS: &[(&str, &str)] = &[
    ("Setup > Appearance", "window.ui_customization"),
    ("Setup > Hardware...", "window.playback_engine"),
    ("Setup > Playback Engine...", "window.playback_engine"),
    ("Setup > MIDI Thru...", "window.midi_thru"),
    ("Setup > Channel Routing...", "window.channel_routing"),
    ("Setup > I/O...", "window.io_setup"),
    ("Setup > Keyboard Shortcuts...", "window.shortcuts"),
    ("Setup > Session", "setup.session"),
    ("Event > Beat Detective", "window.beat_detective"),
    ("Event > Tempo Operations > Tempo Operations Window", "window.tempo_ops"),
    ("Event > Time Operations > Time Operations Window", "window.time_ops"),
    ("Event > MIDI Operations > MIDI Operations Window", "window.midi_ops"),
    ("Event > MIDI Real-Time Properties", "window.rtp"),
];

/// The shortcut of an engine or window command.
pub fn shortcut_for(id: &str) -> Option<&'static str> {
    soundcraft_engine::find_command(id).map_or_else(|| UI_COMMANDS.iter().find(|c| c.0 == id).and_then(|c| c.3), |c| c.shortcut)
}

/// Undo and Redo name the step they would undo or redo; other items keep their catalog label.
pub fn live_label(app: &SoundApp, id: Option<&str>) -> Option<String> {
    let e = &app.engine;
    match id? {
        "edit.undo" => Some(e.undo_label().map_or_else(|| "Can't Undo".into(), |l| format!("Undo {l}"))),
        "edit.redo" => Some(e.redo_label().map_or_else(|| "Can't Redo".into(), |l| format!("Redo {l}"))),
        _ => None,
    }
}

/// Menu-style invocation: may open a dialog.
pub fn invoke_menu(app: &mut SoundApp, id: &str, path: &str) {
    let path = display_path(path);
    if let Some((_, w)) = MENU_WINDOWS.iter().find(|(p, _)| *p == path)
        && w.starts_with("window.")
    {
        let _ = app.run(w, json!({"value": true}));
        return;
    }
    if id == "audiosuite.process" {
        let p = params_for(path, id);
        app.ui.audiosuite = p.get("process").and_then(Value::as_str).map(str::to_string);
        return;
    }
    if wants_dialog(id) && app.dialogs.open_for_command(&app.engine, id) {
        return;
    }
    // These ids only exist to open a dialog from the menu. Programmatic `app.run` refuses them.
    if id == "ui.new_tracks_dialog" {
        let _ = app.dialogs.open_for_command(&app.engine, "track.new");
        return;
    }
    if id == "ui.bounce_dialog" {
        let _ = app.dialogs.open_for_command(&app.engine, "file.bounce_mix");
        return;
    }
    let params = params_for(path, id);
    let _ = crate::dialogs::run_or_ask(app, id, params);
}

/// Keyboard shortcut for a dialog command, or one that would close a modified session: open its
/// dialog. Returns true when handled.
pub fn invoke_shortcut_dialog(app: &mut SoundApp, id: &str) -> bool {
    (wants_dialog(id) && app.dialogs.open_for_command(&app.engine, id)) || crate::dialogs::ask_to_save(app, id, &json!({}))
}

/// Commands that open a dialog first: the listed ones, Score Setup, and anything that needs a path.
fn wants_dialog(id: &str) -> bool {
    DIALOG_COMMANDS.contains(&id) || id == "file.score_setup" || crate::dialogs::takes_path(id)
}

/// Handle UI-layer commands. Returns None when `id` is not a UI command.
pub fn run_ui_command(app: &mut SoundApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if matches!(id, "ui.dock" | "ui.dock.reset") {
        return Some(crate::docking::command(app, id == "ui.dock.reset", p));
    }
    if let Some(v) = crate::extra_windows::run(app, id, p) {
        return Some(Ok(v));
    }
    if id == "ui.audio_device" {
        return Some(crate::windows::audio_device_command(app, p));
    }
    let toggle = |v: &mut bool| {
        *v = p.get("value").and_then(Value::as_bool).unwrap_or(!*v);
        json!({"value": *v})
    };
    let r = match id {
        "window.mix" => {
            app.ui.window = MainWindow::Mix;
            app.focus_mix = app.ui.mix_separate;
            json!({"window": "mix"})
        }
        "window.mix_separate" => {
            let v = toggle(&mut app.ui.mix_separate);
            app.focus_mix = app.ui.mix_separate;
            v
        }
        "window.edit" => {
            app.ui.window = MainWindow::Edit;
            json!({"window": "edit"})
        }
        "window.toggle_mix_edit" => {
            if app.ui.mix_separate {
                // Both are on screen: bring the Mix window forward instead.
                app.focus_mix = true;
            } else {
                app.ui.window = if app.ui.window == MainWindow::Edit { MainWindow::Mix } else { MainWindow::Edit };
            }
            json!({"window": app.ui.window})
        }
        "window.transport" => toggle(&mut app.ui.show_transport),
        "window.big_counter" => toggle(&mut app.ui.show_big_counter),
        "window.memory_locations" => toggle(&mut app.ui.show_memory_locations),
        "window.undo_history" => toggle(&mut app.ui.show_undo_history),
        "window.clip_list" | "window.clip_list_view" => toggle(&mut app.ui.show_clip_list),
        "window.track_list" => toggle(&mut app.ui.show_tracks_list),
        "window.narrow_mix" => toggle(&mut app.ui.narrow_mix),
        "window.session_info" => toggle(&mut app.ui.show_session_info),
        "window.audio_health" => {
            app.audio_health_report = None;
            toggle(&mut app.ui.show_audio_health)
        }
        "window.about" => toggle(&mut app.ui.show_about),
        "window.midi_editor" => toggle(&mut app.ui.show_midi_editor),
        "window.midi_front" => {
            app.ui.show_midi_editor = true;
            json!({"value": true})
        }
        "window.midi_back" => {
            app.ui.show_midi_editor = false;
            json!({})
        }
        "window.automation" => toggle(&mut app.ui.show_automation),
        "window.clip_effects" | "window.clip_effects_dock" => toggle(&mut app.ui.show_clip_effects),
        "window.search" => toggle(&mut app.ui.show_search),
        "window.score" => toggle(&mut app.ui.show_score),
        "window.beat_detective" => toggle(&mut app.ui.show_beat_detective),
        "window.tempo_ops" => toggle(&mut app.ui.show_tempo_ops),
        "window.time_ops" => toggle(&mut app.ui.show_time_ops),
        "window.midi_ops" => toggle(&mut app.ui.show_midi_ops),
        "window.rtp" => toggle(&mut app.ui.show_rtp),
        "window.playback_engine" => toggle(&mut app.ui.show_playback_engine),
        "window.io_setup" => toggle(&mut app.ui.show_io_setup),
        "window.shortcuts" => toggle(&mut app.ui.show_shortcuts),
        "window.color_palette" => toggle(&mut app.ui.show_color_palette),
        "window.disk_usage" => toggle(&mut app.ui.show_disk_usage),
        "window.system_usage" => toggle(&mut app.ui.show_system_usage),
        "window.task_manager" => toggle(&mut app.ui.show_task_manager),
        "window.metadata" => toggle(&mut app.ui.show_metadata),
        "window.event_list" => toggle(&mut app.ui.show_event_list),
        "window.midi_keyboard" => toggle(&mut app.ui.show_midi_keyboard),
        "window.workspace" | "window.workspace_presets" | "window.workspace_sounds" | "window.workspace_front" => {
            app.ui.show_workspace = true;
            json!({"value": true})
        }
        "window.workspace_close" => {
            app.ui.show_workspace = false;
            json!({"value": false})
        }
        "window.configurations" => toggle(&mut app.ui.show_configurations),
        "window.config_new" | "window.config_update" => {
            if let Err(error) = app.ui.docking.validate().and_then(|()| app.ui.mix_docking.validate_mix()) {
                return Some(Err(error));
            }
            let mut snap = app.ui.clone();
            snap.configurations.clear();
            let v = serde_json::to_value(&snap).unwrap_or_default();
            if id == "window.config_update" && !app.ui.configurations.is_empty() {
                if let Some(last) = app.ui.configurations.last_mut() {
                    last.1 = v;
                }
            } else {
                let n = app.ui.configurations.len() + 1;
                app.ui.configurations.push((format!("Configuration {n}"), v));
            }
            json!({"configurations": app.ui.configurations.len()})
        }
        "window.arrange_tile" | "window.arrange_tile_h" | "window.arrange_tile_v" | "window.arrange_cascade" => {
            if let Err(error) = crate::docking::arrange(app) {
                app.ui.status = format!("Panel windows could not be arranged: {error}");
            }
            app.arrange_request = true;
            json!({})
        }
        "window.universe" => toggle(&mut app.ui.show_universe),
        "window.video" => toggle(&mut app.ui.show_video),
        "window.transcript" => {
            if let Some(value) = p.get("docked") {
                let Some(docked) = value.as_bool() else { return Some(Err("docked must be a boolean".into())) };
                app.ui.transcript.docked = docked;
            }
            toggle(&mut app.ui.show_transcript)
        }
        "window.video_universe" => toggle(&mut app.ui.show_video_universe),
        "window.close" | "window.hide_floating" => {
            app.ui.show_transport = false;
            app.ui.show_big_counter = false;
            app.ui.show_memory_locations = false;
            app.ui.show_undo_history = false;
            app.ui.show_session_info = false;
            app.ui.show_audio_health = false;
            app.ui.show_transcript = false;
            app.ui.show_automation = false;
            app.ui.plugin_windows.clear();
            app.ui.audiosuite = None;
            json!({})
        }
        "view.mix_section" => {
            let sec = p.get("section").and_then(Value::as_str).unwrap_or("");
            match sec {
                "all" => {
                    app.ui.mix_views =
                        ["inserts_ae", "inserts_fj", "sends_ae", "sends_fj", "io", "comments", "color"].iter().map(|s| s.to_string()).collect()
                }
                "minimal" => app.ui.mix_views.clear(),
                "" => return Some(Err("`section` required".into())),
                s => {
                    if app.ui.mix_views.iter().any(|v| v == s) {
                        app.ui.mix_views.retain(|v| v != s);
                    } else {
                        app.ui.mix_views.push(s.to_string());
                    }
                }
            }
            json!({"mix_views": app.ui.mix_views})
        }
        "ui.new_tracks_dialog" => {
            return Some(Err("ui.new_tracks_dialog opens a dialog from the menu only; call track.new with parameters".into()));
        }
        "ui.bounce_dialog" => {
            return Some(Err("ui.bounce_dialog opens a dialog from the menu only; call file.bounce_mix with a path".into()));
        }
        "ui.theme" => {
            let Some(mode) = p.get("mode").and_then(Value::as_str) else {
                return Some(Ok(json!({"mode": app.ui.theme.id()})));
            };
            let Some(mode) = crate::theme::ThemeMode::parse(mode) else {
                return Some(Err(format!("unknown theme `{mode}` (system, light or dark)")));
            };
            app.ui.theme = mode;
            json!({"mode": mode.id()})
        }
        "ui.set" => match serde_json::to_value(&app.ui) {
            Ok(mut cur) => {
                if let (Some(obj), Some(patch)) = (cur.as_object_mut(), p.as_object()) {
                    for (k, v) in patch {
                        obj.insert(k.clone(), v.clone());
                    }
                }
                match serde_json::from_value::<crate::UiState>(cur) {
                    Ok(ns) => {
                        if let Err(error) = ns.docking.validate().and_then(|()| ns.mix_docking.validate_mix()) {
                            return Some(Err(error));
                        }
                        crate::docking::cancel_replaced_state(app, &ns);
                        app.ui = ns;
                        json!({"ui": app.ui})
                    }
                    Err(e) => return Some(Err(format!("bad ui state: {e}"))),
                }
            }
            Err(e) => return Some(Err(e.to_string())),
        },
        _ => return None,
    };
    Some(Ok(r))
}

/// Parity including UI-layer commands.
pub fn parity() -> Value {
    let extra = parity_aliases();
    catalog::parity_with(&extra)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, SoundApp};

    fn leaves(nodes: &[MenuNode], out: &mut Vec<String>) {
        for n in nodes {
            if n.children.is_empty() {
                out.push(n.path.clone());
            } else {
                leaves(&n.children, out);
            }
        }
    }

    fn labels(nodes: &[MenuNode], out: &mut Vec<String>) {
        for n in nodes {
            out.push(n.label.clone());
            labels(&n.children, out);
        }
    }

    /// App-level parity (engine plus UI aliases) before the rebrand: 482 / 512. It only ever rises.
    const PARITY_FLOOR: u64 = 482;

    /// Incumbent plug-in and product names that must not reach a Pa Tools menu.
    const INCUMBENT_NAMES: &[&str] = &[
        "VENUE",
        "EQ3",
        "BF-76",
        "Dyn3",
        "Maxim",
        "Pitch II",
        "Pitch Shift Legacy",
        "Vari-Fi",
        "D-Verb",
        "Mod Delay III",
        "Sci-Fi",
        "Eleven",
        "Recti-Fi",
        "SansAmp",
        "Sibelius",
        "Ableton",
        "Soundbase",
        "Pro Tools",
        "Melodyne",
        "Dolby",
        "Avid",
        "SoundCraft",
        "ArtCraft",
    ];

    /// `name` appears in `text` as a whole word ("Maxim" is not in "Maximizer").
    fn mentions(text: &str, name: &str) -> bool {
        text.match_indices(name).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + name.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        })
    }

    #[test]
    fn renamed_entries_are_consistent_with_the_catalog() {
        let catalog = catalog::catalog();
        for (cat, shown) in RENAMED {
            assert!(catalog.contains(cat), "{cat} is not a catalog entry");
            assert!(!catalog.contains(shown), "{shown} must not be a catalog entry");
            assert_eq!(display_path(cat), *shown);
            assert_eq!(catalog_path(shown), *cat);
            assert!(!is_third_party_mark(shown), "{shown} still names a third party");
        }
        assert_eq!(display_path("File > Save Session"), "File > Save Session", "unrenamed paths pass through");
        assert_eq!(catalog_path("File > Save Session"), "File > Save Session");
        assert_eq!((AUDIOSUITE.len(), PARITY_ALIASES.len()), (28, 28));
        for ((shown, process), (cat, id)) in AUDIOSUITE.iter().zip(PARITY_ALIASES) {
            assert!(catalog.contains(cat), "{cat} is not a catalog entry");
            assert_eq!(*id, "audiosuite.process");
            assert_eq!(display_path(cat), *shown, "AUDIOSUITE and PARITY_ALIASES list the same entries in the same order");
            assert!(!process.is_empty());
        }
        // 16 incumbent AudioSuite names, the 6 other renames in `RENAMED`.
        assert_eq!(RENAMED.len(), 22);
        assert_eq!(RENAMED.iter().filter(|(c, _)| c.starts_with("AudioSuite")).count(), 16);
    }

    #[test]
    fn audiosuite_menu_shows_only_our_own_names() {
        for (shown, _) in AUDIOSUITE {
            for bad in INCUMBENT_NAMES {
                assert!(!mentions(shown, bad), "{shown} contains {bad}");
            }
        }
        let mut shown = Vec::new();
        leaves(&tree(), &mut shown);
        for process in ["AudioSuite > EQ > Pa EQ 1-Band", "AudioSuite > Harmonic > Pa Saturator", "AudioSuite > Pitch Shift > Pa Varispeed"] {
            assert!(shown.iter().any(|p| p == process), "{process} missing from the menu");
        }
        assert!(!shown.iter().any(|p| p.contains("D-Verb") || p.contains("SansAmp")));
        assert!(mentions("AudioSuite > Dynamics > Maxim", "Maxim") && !mentions("Pa Maximizer", "Maxim"));
    }

    #[test]
    fn the_menu_tree_carries_no_third_party_marks() {
        let mut all = Vec::new();
        labels(&tree(), &mut all);
        for label in &all {
            assert!(!is_third_party_mark(label), "menu label {label:?} names a third party");
            for bad in INCUMBENT_NAMES {
                assert!(!mentions(label, bad), "menu label {label:?} contains {bad}");
            }
        }
        // The same filter keeps ordinary items: nothing implemented was dropped.
        let mut shown = Vec::new();
        leaves(&tree(), &mut shown);
        let hidden = hidden_leaves();
        let mut list: Vec<&str> = hidden.iter().copied().collect();
        list.sort_unstable();
        let mut expected = vec![
            "Clip > ARA Plugins > Melodyne",
            "Edit > Automation > Duplicate Dolby Atmos Plugin Automation to Pan Automation",
            "Edit > Automation > Duplicate Pan Automation to Dolby Atmos Plugin Automation",
            "File > Export > Selected Tracks as New Media Composer Compatible Session...",
            "File > Import > VENUE Channel Names as Track Names",
            "File > Open > Sketch...",
            "Options > Activate HEAT",
            "Track > ARA Plugins > Melodyne",
            "View > Mix Window Views > HEAT",
            "Window > ARA Plugins",
            "Window > Collaboration > Pro Tools Projects",
            "Window > Sketch to Front",
            "Window > SoundFlow",
            "Window > Splice",
        ];
        expected.sort_unstable();
        assert_eq!(list, expected, "the leaves left out of the menu bar");
        for path in hidden {
            assert!(catalog::implemented_by(path, &parity_aliases()).is_none(), "{path} is implemented but hidden");
        }
        let kept = catalog::catalog().iter().filter(|c| !hidden.contains(*c)).count();
        // Catalog leaves shown, plus the UI-only items `tree()` adds beyond the catalog.
        assert!(shown.len() >= kept, "{} shown, {kept} expected", shown.len());
        for implemented in [
            "File > Export > Notation Program (MusicXML)...",
            "File > Send To... > Notation Program",
            "View > Transport > Link Sync",
            "View > Clip > Note Overlay",
        ] {
            assert!(shown.iter().any(|p| p == implemented), "{implemented} missing");
            assert!(command_for_path(implemented, &ui_aliases()).is_some(), "{implemented} lost its command");
        }
        for window in ["Window > Pa Tools Search", "Window > New Workspace > Sound Browser"] {
            assert!(command_for_path(window, &ui_aliases()).is_some(), "{window} lost its command");
        }
    }

    #[test]
    fn third_party_marks_are_found() {
        for label in
            ["Dolby Atmos Plugin", "Activate HEAT", "Sketch...", "Splice", "SoundFlow", "Pro Tools Projects", "Melodyne", "ARA Plugins", "Soundbase"]
        {
            assert!(is_third_party_mark(label), "{label}");
        }
        for label in ["Link Sync", "Pa Tools Search", "Notation Program", "Heat Map", "Paragraph", "Gain", "Track Color", "AUDIO"] {
            assert!(!is_third_party_mark(label), "{label}");
        }
    }

    #[test]
    fn renaming_does_not_lower_the_parity_measure() {
        let catalog = catalog::catalog();
        let aliases = parity_aliases();
        for (cat, _) in PARITY_ALIASES {
            assert_eq!(catalog::implemented_by(cat, &aliases).as_deref(), Some("audiosuite.process"), "{cat}");
        }
        for cat in
            ["File > Export > Sibelius...", "File > Send To... > Sibelius", "View > Transport > Ableton Link", "View > Clip > ARA Note Overlay"]
        {
            assert!(catalog::implemented_by(cat, &aliases).is_some(), "{cat} stopped counting");
        }
        for cat in ["Window > Pro Tools Search", "Window > New Workspace > Soundbase"] {
            assert!(catalog::implemented_by(cat, &aliases).is_some(), "{cat} stopped counting");
        }
        let p = parity();
        assert_eq!(p["total"], catalog.len());
        assert_eq!(p["total"], 512, "the catalog is untouched");
        assert!(p["implemented"].as_u64().unwrap() >= PARITY_FLOOR, "app-level parity regressed: {}", p["implemented"]);
        // The menu-facing aliases are keyed by our names, so they do not count catalog items by themselves.
        let by_display = catalog::parity_with(&ui_aliases());
        assert!(by_display["implemented"].as_u64().unwrap() < p["implemented"].as_u64().unwrap());
    }

    #[test]
    fn both_spellings_of_a_renamed_audiosuite_item_run_the_same_process() {
        let mut app = SoundApp::new(soundcraft_engine::Engine::default(), None, Services::default());
        invoke_menu(&mut app, "audiosuite.process", "AudioSuite > Harmonic > SansAmp PSA-1");
        assert_eq!(app.ui.audiosuite.as_deref(), Some("saturator"));
        app.ui.audiosuite = None;
        invoke_menu(&mut app, "audiosuite.process", "AudioSuite > Harmonic > Pa Saturator");
        assert_eq!(app.ui.audiosuite.as_deref(), Some("saturator"));
        invoke_menu(&mut app, "audiosuite.process", "AudioSuite > Dynamics > Pa Maximizer");
        assert_eq!(app.ui.audiosuite.as_deref(), Some("maximizer"));
    }

    #[test]
    fn the_about_item_carries_the_product_name() {
        let about = UI_COMMANDS.iter().find(|c| c.0 == "window.about").map(|c| c.1);
        assert_eq!(about, Some(format!("About {}", soundcraft_brand::APP_NAME).as_str()));
        let search = UI_COMMANDS.iter().find(|c| c.0 == "window.search").map(|c| c.2);
        assert_eq!(search, Some("Window > Pa Tools Search"));
    }

    #[test]
    fn mix_view_aliases_are_allocated_once() {
        let first = ui_aliases();
        let second = ui_aliases();
        let a = first.iter().find(|(path, _)| path.contains("Mix Window Views > All")).map(|(path, _)| *path).unwrap();
        let b = second.iter().find(|(path, _)| path.contains("Mix Window Views > All")).map(|(path, _)| *path).unwrap();
        assert!(std::ptr::eq(a, b));
        assert_eq!(first.iter().filter(|(path, _)| path.contains("Mix Window Views")).count(), 14);
    }

    #[test]
    fn programmatic_dialog_commands_stay_closed_and_the_menu_opens_them() {
        let mut app = SoundApp::new(soundcraft_engine::Engine::default(), None, Services::default());
        let err = app.run("ui.new_tracks_dialog", json!({})).unwrap_err();
        assert!(err.contains("track.new"), "{err}");
        let err = app.run("ui.bounce_dialog", json!({})).unwrap_err();
        assert!(err.contains("file.bounce_mix"), "{err}");
        assert!(app.dialogs.open_name().is_none());

        invoke_menu(&mut app, "ui.new_tracks_dialog", "Track > New...");
        assert_eq!(app.dialogs.open_name(), Some("new_tracks"));
        app.dialogs.open = None;
        invoke_menu(&mut app, "ui.bounce_dialog", "File > Bounce Mix...");
        assert_eq!(app.dialogs.open_name(), Some("bounce"));
    }

    #[test]
    fn save_asks_for_a_path_until_the_session_has_one() {
        let mut app = SoundApp::new(soundcraft_engine::Engine::default(), None, Services::default());
        invoke_menu(&mut app, "session.save", "File > Save Session");
        assert!(matches!(&app.dialogs.open, Some(crate::dialogs::Dialog::PathPrompt { cmd, .. }) if cmd == "session.save_as"));
        app.dialogs.open = None;
        assert!(invoke_shortcut_dialog(&mut app, "session.save"));
        app.dialogs.open = None;

        let dir = std::env::temp_dir().join(format!("soundcraft-save-{}", std::process::id()));
        let path = dir.join("Song.scraft");
        app.run("session.save_as", json!({"path": path.to_string_lossy()})).unwrap();
        app.run("track.new", json!({})).unwrap();
        assert!(!invoke_shortcut_dialog(&mut app, "session.save"));
        invoke_menu(&mut app, "session.save", "File > Save Session");
        assert!(app.dialogs.open.is_none());
        assert!(!app.engine.is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn track_instrument_menu_targets_the_selected_instrument_track() {
        let mut e = soundcraft_engine::Engine::default();
        let audio = e.execute("track.new", &json!({"kind": "audio"})).unwrap()["tracks"][0].as_u64().unwrap();
        assert_eq!(instrument_track(e.session()), None);
        let inst = e.execute("track.new", &json!({"kind": "instrument"})).unwrap()["tracks"][0].as_u64().unwrap();
        e.execute("edit.select", &json!({"tracks": [audio]})).unwrap();
        assert_eq!(instrument_track(e.session()), None, "an audio track has no instrument");
        e.execute("edit.select", &json!({"tracks": [audio, inst]})).unwrap();
        assert_eq!(instrument_track(e.session()).map(|t| t.0), Some(inst));
    }
}
