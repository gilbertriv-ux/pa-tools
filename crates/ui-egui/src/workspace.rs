//! Desktop workspace navigation and session onboarding, inspired by the V2 design study.

use egui::{Frame, Margin, RichText, Ui};
use serde_json::json;

use crate::{MainWindow, SoundApp, menus, theme::Tokens};

pub fn show(app: &mut SoundApp, ui: &mut Ui) {
    let t = Tokens::current();
    egui::Panel::top("workspace_header").frame(Frame::NONE.fill(t.panel_bg).inner_margin(Margin::symmetric(12, 7))).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(soundcraft_brand::APP_NAME.to_uppercase()).strong().color(t.mode_on));
            ui.separator();
            let session = app.engine.session();
            let name_width = (ui.available_width() - 215.0).max(40.0);
            ui.add_sized([name_width, 18.0], egui::Label::new(RichText::new(&session.name).strong()).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{} tracks  ·  {} Hz", session.tracks.len(), session.sample_rate.hz())).small().color(t.text_dim));
            });
        });
        ui.add_space(5.0);
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(app.ui.window == MainWindow::Edit, "Edit Timeline").clicked() {
                let _ = app.run("window.edit", json!({}));
            }
            if ui.selectable_label(app.ui.window == MainWindow::Mix, "Mix Console").clicked() {
                let _ = app.run("window.mix", json!({}));
            }
            if ui.selectable_label(app.ui.show_transcript, "Transcript").clicked() {
                let _ = app.run("window.transcript", json!({"docked": true}));
            }
            ui.separator();
            for (label, command) in
                [("Add Track", "track.new"), ("Import Audio", "file.import_audio"), ("Open Session", "session.open"), ("Save", "session.save")]
            {
                if ui.button(label).clicked() {
                    menus::invoke_menu(app, command, "");
                }
            }
        });
    });
}

/// These use the existing menu invocation path, including unsaved-session protection.
pub(crate) fn empty_session(app: &mut SoundApp, ui: &mut Ui) {
    let t = Tokens::current();
    ui.vertical_centered(|ui| {
        ui.add_space(36.0);
        ui.heading("Start your session");
        ui.label(RichText::new("Add a track, import audio, or open a saved session.").color(t.text_dim));
        ui.add_space(16.0);
        for (label, command) in
            [("Create an Audio Track", "track.new"), ("Import an Audio File", "file.import_audio"), ("Open a Saved Session", "session.open")]
        {
            if ui.add_sized([210.0, 30.0], egui::Button::new(label)).clicked() {
                menus::invoke_menu(app, command, "");
            }
            ui.add_space(5.0);
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Edit and mix audio here. Open Transcript to transcribe a spoken track.").small().color(t.text_dim));
    });
}
