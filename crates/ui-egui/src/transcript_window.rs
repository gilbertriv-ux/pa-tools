//! Searchable per-track transcript. Word positions use session samples throughout.

use egui::{RichText, ScrollArea, vec2};
use serde_json::json;
use soundcraft_model::{TrackId, TrackKind};
use soundcraft_time::Samples;

use crate::SoundApp;

pub struct Job {
    receiver: std::sync::mpsc::Receiver<Result<soundcraft_model::Transcript, String>>,
    revision: u64,
}

/// Poll recognition without blocking a frame. Results never replace a changed session's data.
pub fn step(app: &mut SoundApp, ctx: &egui::Context) {
    let Some(job) = &app.transcription else { return };
    let result = match job.receiver.try_recv() {
        Ok(result) => result,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("Transcription worker stopped".into()),
    };
    let revision = job.revision;
    app.transcription = None;
    app.ui.status = match result {
        Ok(transcript) if revision == app.engine.revision => {
            let count = transcript.words.len();
            app.engine.session_mut().set_transcript(transcript);
            format!("Transcribed {count} words")
        }
        Ok(_) => "The session changed during transcription. Transcribe the track again.".into(),
        Err(error) => error,
    };
}

#[cfg(not(target_arch = "wasm32"))]
fn start(app: &mut SoundApp, track: TrackId, ctx: &egui::Context) {
    let session = app.engine.session().clone();
    let revision = app.engine.revision;
    let (sender, receiver) = std::sync::mpsc::channel();
    let context = ctx.clone();
    let worker = std::thread::Builder::new().name("speech-transcription".into()).spawn(move || {
        let mut engine = soundcraft_engine::Engine::new(session);
        let result = engine.execute("track.transcribe", &json!({"track": track.0})).map_err(|e| e.to_string()).and_then(|value| {
            if value["status"] == "ok" {
                engine.session().transcript(track).cloned().ok_or_else(|| "No transcript was produced".into())
            } else {
                Err(value["error"].as_str().or(value["hint"].as_str()).unwrap_or("Speech model is not installed").into())
            }
        });
        let _ = sender.send(result);
        context.request_repaint();
    });
    match worker {
        Ok(_) => {
            app.transcription = Some(Job { receiver, revision });
            app.ui.status = "Transcribing track…".into();
            ctx.request_repaint();
        }
        Err(error) => app.ui.status = format!("Could not start transcription: {error}"),
    }
}

#[cfg(target_arch = "wasm32")]
fn start(app: &mut SoundApp, _track: TrackId, _ctx: &egui::Context) {
    app.ui.status = "Local speech recognition requires the desktop app".into();
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct State {
    pub track: Option<TrackId>,
    pub search: String,
    pub follow_playhead: bool,
}

impl Default for State {
    fn default() -> Self {
        Self { track: None, search: String::new(), follow_playhead: true }
    }
}

/// Human-readable session time, rounded to milliseconds (no floating-point seek conversion).
pub fn format_position(samples: Samples, sample_rate: u32) -> String {
    let ms = ((samples.max(0) as u128 * 1000 + u128::from(sample_rate) / 2) / u128::from(sample_rate.max(1))).min(u64::MAX as u128) as u64;
    let (hours, minutes, seconds, fraction) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if hours > 0 { format!("{hours:02}:{minutes:02}:{seconds:02}.{fraction:03}") } else { format!("{minutes:02}:{seconds:02}.{fraction:03}") }
}

pub fn show(app: &mut SoundApp, ctx: &egui::Context) {
    let mut open = app.ui.show_transcript;
    if !open {
        return;
    }
    let tracks: Vec<_> = app.engine.session().tracks.iter().filter(|t| t.kind == TrackKind::Audio).map(|t| (t.id, t.name.clone())).collect();
    if !tracks.iter().any(|(id, _)| Some(*id) == app.ui.transcript.track) {
        app.ui.transcript.track = app
            .engine
            .session()
            .edit
            .selected_tracks
            .iter()
            .copied()
            .find(|id| tracks.iter().any(|(t, _)| t == id))
            .or_else(|| tracks.first().map(|t| t.0));
    }
    let position = app.position();
    let sr = app.engine.session().sample_rate.hz();
    let mut seek = None;
    let mut transcribe = false;
    egui::Window::new("Transcript").open(&mut open).default_size(vec2(620.0, 440.0)).show(ctx, |ui| {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("transcript_track")
                .selected_text(tracks.iter().find(|(id, _)| Some(*id) == app.ui.transcript.track).map_or("No audio track", |(_, name)| name))
                .show_ui(ui, |ui| {
                    for (id, name) in &tracks {
                        ui.selectable_value(&mut app.ui.transcript.track, Some(*id), name);
                    }
                });
            transcribe = ui
                .add_enabled(
                    cfg!(all(feature = "whisper", not(target_arch = "wasm32"))) && app.ui.transcript.track.is_some() && app.transcription.is_none(),
                    egui::Button::new("Transcribe Track"),
                )
                .on_disabled_hover_text("Select an audio track in a desktop build with local transcription enabled.")
                .clicked();
        });
        if app.transcription.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Transcribing track…");
            });
        }
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.text_edit_singleline(&mut app.ui.transcript.search);
            ui.checkbox(&mut app.ui.transcript.follow_playhead, "Follow Playhead");
        });
        ui.separator();
        let transcript = app.ui.transcript.track.and_then(|id| app.engine.session().transcript(id));
        let Some(transcript) = transcript else {
            ui.label("No transcript yet. Select an audio track and transcribe it.");
            return;
        };
        ui.label(format!("{} words · {}", transcript.words.len(), transcript.language));
        let search = app.ui.transcript.search.to_lowercase();
        ScrollArea::vertical().id_salt("transcript_words").auto_shrink([false; 2]).show(ui, |ui| {
            for word in &transcript.words {
                if !search.is_empty() && !word.text.to_lowercase().contains(&search) {
                    continue;
                }
                let active = position >= word.start && position < word.end;
                let row = ui.horizontal(|ui| {
                    ui.label(RichText::new(format_position(word.start, sr)).monospace());
                    ui.label(format!("Speaker {}", u16::from(word.speaker) + 1));
                    if ui.selectable_label(active, &word.text).clicked() {
                        seek = Some(word.start);
                    }
                });
                if active && app.ui.transcript.follow_playhead {
                    row.response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });
    });
    app.ui.show_transcript = open;
    if let Some(at) = seek {
        let _ = app.run("transport.locate", json!({"at": at}));
    }
    if transcribe && let Some(track) = app.ui.transcript.track {
        start(app, track, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed_job(app: &mut SoundApp) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let transcript = soundcraft_model::Transcript { track: TrackId(1), words: Vec::new(), language: "en".into(), source: "test".into() };
        assert!(sender.send(Ok(transcript)).is_ok());
        app.transcription = Some(Job { receiver, revision: app.engine.revision });
    }

    #[test]
    fn a_completed_job_cannot_write_into_a_replaced_session() {
        let mut app = SoundApp::new(soundcraft_engine::Engine::default(), None, crate::Services::default());
        completed_job(&mut app);
        app.engine.replace_session(soundcraft_model::Session::default());
        step(&mut app, &egui::Context::default());
        assert!(app.engine.session().transcripts.is_empty());
        assert!(app.ui.status.contains("session changed"));
        assert!(app.transcription.is_none());
    }

    #[test]
    fn a_completed_job_is_stored_when_its_session_is_unchanged() {
        let mut app = SoundApp::new(soundcraft_engine::Engine::default(), None, crate::Services::default());
        completed_job(&mut app);
        step(&mut app, &egui::Context::default());
        assert!(app.engine.session().transcript(TrackId(1)).is_some());
        assert!(app.transcription.is_none());
    }
}
