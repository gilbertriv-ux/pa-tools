//! Searchable per-track transcript. Word positions use session samples throughout.

use egui::{RichText, ScrollArea, vec2};
use serde_json::json;
use soundcraft_model::{TrackId, TrackKind, TranscriptWord};
use soundcraft_time::Samples;

use crate::{SoundApp, theme::Tokens};

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
    app.ui.transcript.error = result.as_ref().err().cloned();
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
            app.ui.transcript.error = None;
            app.transcription = Some(Job { receiver, revision });
            app.ui.status = "Transcribing track…".into();
            ctx.request_repaint();
        }
        Err(error) => {
            app.ui.status = format!("Could not start transcription: {error}");
            app.ui.transcript.error = Some(app.ui.status.clone());
        }
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
    pub docked: bool,
    #[serde(skip)]
    pub error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self { track: None, search: String::new(), follow_playhead: true, docked: true, error: None }
    }
}

/// Human-readable session time, rounded to milliseconds (no floating-point seek conversion).
pub fn format_position(samples: Samples, sample_rate: u32) -> String {
    let ms = ((samples.max(0) as u128 * 1000 + u128::from(sample_rate) / 2) / u128::from(sample_rate.max(1))).min(u64::MAX as u128) as u64;
    let (hours, minutes, seconds, fraction) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if hours > 0 { format!("{hours:02}:{minutes:02}:{seconds:02}.{fraction:03}") } else { format!("{minutes:02}:{seconds:02}.{fraction:03}") }
}

/// Adapted from the AI Studio grouping study. Original word indices and timestamps are retained.
/// A zero rate keeps every word visible as an individual card; it never invents a timebase.
pub fn utterance_ranges(words: &[TranscriptWord], sample_rate: u32) -> Vec<std::ops::Range<usize>> {
    let mut result = Vec::new();
    let mut first = 0;
    let mut first_start = 0;
    let mut previous: Option<&TranscriptWord> = None;
    let rate = i64::from(sample_rate);
    for (index, word) in words.iter().enumerate() {
        if let Some(prev) = previous {
            let split = sample_rate == 0
                || word.speaker != prev.speaker
                || index.saturating_sub(first) >= 40
                || word.start < prev.start
                || word.start.saturating_sub(prev.end) > rate * 3 / 2
                || word.end.saturating_sub(first_start) > rate * 15;
            if split {
                result.push(first..index);
                first = index;
                first_start = word.start;
            }
        } else {
            first_start = word.start;
        }
        previous = Some(word);
    }
    if previous.is_some() {
        result.push(first..words.len());
    }
    result
}

const DOCK_MIN_WIDTH: f32 = 1040.0;

pub fn show_docked(app: &mut SoundApp, ui: &mut egui::Ui) {
    if !app.ui.show_transcript
        || !app.ui.transcript.docked
        || ui.ctx().content_rect().width() < DOCK_MIN_WIDTH
        || ui.ctx().viewport_id() != egui::ViewportId::ROOT
    {
        return;
    }
    let t = Tokens::current();
    egui::Panel::right("transcript_inspector")
        .resizable(true)
        .default_size(380.0)
        .size_range(300.0..=540.0)
        .frame(egui::Frame::NONE.fill(t.panel_bg).inner_margin(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Transcript Inspector").strong().color(t.mode_on));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Close").on_hover_text("Close Transcript").clicked() {
                        app.ui.show_transcript = false;
                    }
                    if ui.small_button("Detach").on_hover_text("Open Transcript in a floating window").clicked() {
                        app.ui.transcript.docked = false;
                    }
                });
            });
            ui.separator();
            content(app, ui);
        });
}

pub fn show(app: &mut SoundApp, ctx: &egui::Context) {
    if !app.ui.show_transcript || (app.ui.transcript.docked && ctx.content_rect().width() >= DOCK_MIN_WIDTH) {
        return;
    }
    let mut open = app.ui.show_transcript;
    egui::Window::new("Transcript")
        .open(&mut open)
        .default_size(vec2(620.0, 520.0))
        .min_size(vec2(280.0, 250.0))
        .max_size(ctx.content_rect().size() - vec2(24.0, 24.0))
        .show(ctx, |ui| {
            if !app.ui.transcript.docked && ui.small_button("Dock Inspector").clicked() {
                app.ui.transcript.docked = true;
            }
            content(app, ui);
        });
    app.ui.show_transcript = open;
}

fn content(app: &mut SoundApp, ui: &mut egui::Ui) {
    let t = Tokens::current();
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
    ui.scope(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("TRACK").small().color(t.text_dim));
            egui::ComboBox::from_id_salt("transcript_track")
                .width((ui.available_width() - 58.0).clamp(100.0, 240.0))
                .selected_text(tracks.iter().find(|(id, _)| Some(*id) == app.ui.transcript.track).map_or("No audio track", |(_, name)| name))
                .show_ui(ui, |ui| {
                    for (id, name) in &tracks {
                        ui.selectable_value(&mut app.ui.transcript.track, Some(*id), name);
                    }
                });
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            transcribe = ui
                .add_enabled(
                    cfg!(all(feature = "whisper", not(target_arch = "wasm32"))) && app.ui.transcript.track.is_some() && app.transcription.is_none(),
                    egui::Button::new(RichText::new("Transcribe Track").strong()),
                )
                .on_disabled_hover_text("Select an audio track in a desktop build with local transcription enabled.")
                .clicked();
            ui.checkbox(&mut app.ui.transcript.follow_playhead, "Follow Playhead");
        });
        if app.transcription.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Transcribing track…");
            });
        }
        if let Some(error) = &app.ui.transcript.error {
            ui.colored_label(t.rec, error);
        }
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(
                egui::TextEdit::singleline(&mut app.ui.transcript.search)
                    .hint_text("Search transcript…")
                    .desired_width((ui.available_width() - 50.0).max(60.0)),
            );
            if ui.small_button("Clear").clicked() {
                app.ui.transcript.search.clear();
            }
        });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("PLAYHEAD").small().color(t.text_dim));
            ui.label(RichText::new(format_position(position, sr)).monospace().color(t.mode_on));
            ui.label(RichText::new(format!("{position} samples")).small().color(t.text_dim));
        });
        let transcript = app.ui.transcript.track.and_then(|id| app.engine.session().transcript(id));
        let Some(transcript) = transcript else {
            ui.add_space(22.0);
            ui.label(RichText::new("Your spoken audio, searchable.").strong());
            ui.label("Select an audio track, then choose Transcribe Track. Click a word to move the playhead to its exact session position.");
            return;
        };
        let search = app.ui.transcript.search.to_lowercase();
        let matches = transcript.words.iter().filter(|word| !search.is_empty() && word.text.to_lowercase().contains(&search)).count();
        let groups = utterance_ranges(&transcript.words, sr);
        ui.label(
            RichText::new(format!("{} words · {} utterances · {}", transcript.words.len(), groups.len(), transcript.language))
                .small()
                .color(t.text_dim),
        );
        if !search.is_empty() {
            ui.label(RichText::new(format!("Matches: {matches}")).small().color(t.accent));
        }
        let active = transcript.words.iter().position(|word| position >= word.start && position < word.end);
        ui.add_space(6.0);
        ScrollArea::vertical().id_salt(("transcript_words", transcript.track.0)).auto_shrink([false; 2]).show(ui, |ui| {
            for range in groups {
                let Some(words) = transcript.words.get(range.clone()) else { continue };
                let (Some(first), Some(last)) = (words.first(), words.last()) else { continue };
                if !search.is_empty() && !words.iter().any(|word| word.text.to_lowercase().contains(&search)) {
                    continue;
                }
                let speaker_color = match first.speaker % 3 {
                    0 => t.accent,
                    1 => t.auto_touch,
                    _ => t.mode_on,
                };
                let card_active = active.is_some_and(|index| range.contains(&index));
                let card = ui.push_id((transcript.track.0, range.start), |ui| {
                    egui::Frame::new()
                        .fill(t.panel_bg2)
                        .corner_radius(6)
                        .stroke(egui::Stroke::new(1.0, if card_active { speaker_color } else { t.border_light }))
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(format!("Speaker {}", u16::from(first.speaker) + 1)).strong().color(speaker_color));
                                ui.label(
                                    RichText::new(format!("{} → {}", format_position(first.start, sr), format_position(last.end, sr)))
                                        .small()
                                        .monospace()
                                        .color(t.text_dim),
                                );
                            });
                            ui.add_space(5.0);
                            let word_width = (ui.available_width() - ui.spacing().button_padding.x * 2.0).max(1.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(3.0, 5.0);
                                for (index, word) in (range.start..range.end).zip(words) {
                                    // Keep words as direct wrap-layout children. An extra scope per
                                    // word inherits wrapping and lets its own minimum rect expand
                                    // the card. The stable card ID and unchanged word order already
                                    // provide stable automatic button IDs.
                                    let found = !search.is_empty() && word.text.to_lowercase().contains(&search);
                                    let active_word = active == Some(index);
                                    let color = if active_word {
                                        t.mode_on_text
                                    } else if found {
                                        speaker_color
                                    } else {
                                        t.text
                                    };
                                    // Lay out each word against a full row, rather than the small
                                    // remainder at the end of the current row. Oversized tokens
                                    // elide onto one line and retain their full text for hover/AT.
                                    let mut layout = egui::text::LayoutJob::simple_singleline(
                                        word.text.clone(),
                                        egui::TextStyle::Button.resolve(ui.style()),
                                        color,
                                    );
                                    layout.wrap.max_width = word_width;
                                    layout.wrap.max_rows = 1;
                                    let text = ui.fonts_mut(|fonts| fonts.layout_job(layout));
                                    let button = egui::Button::new(text)
                                        .wrap_mode(egui::TextWrapMode::Extend)
                                        .selected(active_word)
                                        .frame(found || active_word);
                                    let button = if active_word { button.fill(t.mode_on) } else { button };
                                    if ui
                                        .add(button)
                                        .on_hover_text(format!("{}\n{} · {} samples", word.text, format_position(word.start, sr), word.start))
                                        .clicked()
                                    {
                                        seek = Some(word.start);
                                    }
                                }
                            });
                        })
                });
                if card_active && app.ui.transcript.follow_playhead {
                    card.response.scroll_to_me(Some(egui::Align::Center));
                }
                ui.add_space(8.0);
            }
        });
    });
    if let Some(at) = seek {
        let _ = app.run("transport.locate", json!({"at": at}));
    }
    if transcribe && let Some(track) = app.ui.transcript.track {
        start(app, track, &ui.ctx().clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start: i64, end: i64, speaker: u8) -> TranscriptWord {
        TranscriptWord { start, end, text: "word".into(), speaker }
    }

    #[test]
    fn utterances_preserve_words_and_split_at_speakers_gaps_and_duration_limits() {
        for (words, rate, expected) in [
            (vec![], 48_000, vec![]),
            (vec![word(0, 10, 0)], 48_000, std::iter::once(0..1).collect::<Vec<_>>()),
            (vec![word(0, 10, 0), word(10, 20, 1), word(20, 30, 1)], 48_000, vec![0..1, 1..3]),
            (vec![word(0, 100, 0), word(72_100, 72_200, 0), word(144_201, 144_300, 0)], 48_000, vec![0..2, 2..3]),
            (vec![word(0, 100, 0), word(100, 720_000, 0), word(720_000, 720_001, 0)], 48_000, vec![0..2, 2..3]),
            (vec![word(i64::MIN, i64::MIN + 1, 0), word(i64::MAX - 1, i64::MAX, 0)], 48_000, vec![0..1, 1..2]),
            (vec![word(10, 20, 0), word(-10, 0, 0)], 48_000, vec![0..1, 1..2]),
            (vec![word(0, 10, 0), word(10, 20, 0)], 0, vec![0..1, 1..2]),
        ] {
            let ranges = utterance_ranges(&words, rate);
            assert_eq!(ranges, expected);
            assert_eq!(ranges.into_iter().flatten().collect::<Vec<_>>(), (0..words.len()).collect::<Vec<_>>());
        }
        let words = vec![word(0, 10, 0); 81];
        assert_eq!(utterance_ranges(&words, u32::MAX), vec![0..40, 40..80, 80..81]);
    }

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
