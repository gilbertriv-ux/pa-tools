//! Window ▸ Transcript, the in-app model download (feature `download`): the window offers a
//! download only once a transcription has found the model missing, drives it on a worker thread with
//! progress on the bar, honours Cancel with the one-long-job-at-a-time discipline, and clears the
//! offer when the model is ready. No real network: tests inject a downloader (the seam
//! [`soundcraft_ui_egui::transcript_window::TranscriptState::inject_downloader`]).
//!
//! Compiles to nothing without the feature.
#![cfg(feature = "download")]

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Shape, epaint::ClippedShape, vec2};
use serde_json::json;
use soundcraft_pa_speech::{SpeechError, models};
use soundcraft_ui_egui::transcript_window::Downloader;
use soundcraft_ui_egui::{Services, SoundApp};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Rig {
    ctx: egui::Context,
    app: SoundApp,
    shapes: Vec<ClippedShape>,
}

impl Rig {
    fn new() -> Rig {
        let app = SoundApp::new(soundcraft_engine::demo::demo_engine(), None, Services::default());
        let mut r = Rig { ctx: egui::Context::default(), app, shapes: Vec::new() };
        for _ in 0..4 {
            r.frame(vec![]);
        }
        r
    }

    fn frame(&mut self, mut events: Vec<Event>) {
        events.insert(0, Event::ModifiersChanged(Modifiers::NONE));
        let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0))), events, ..Default::default() };
        let mut out = self.ctx.run_ui(raw, |ui| {
            self.app.logic(ui.ctx());
            self.app.ui(ui);
        });
        out.textures_delta.clear();
        self.shapes = out.shapes;
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(vec![]);
        }
    }

    fn texts(&self) -> Vec<String> {
        self.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    fn find(&self, text: &str) -> Option<Rect> {
        self.shapes.iter().find_map(|c| match &c.shape {
            Shape::Text(t) if t.galley.text() == text => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        })
    }

    /// The first drawn text that starts with `prefix` (the Download button carries a size that we do
    /// not want to hard-code).
    fn find_prefix(&self, prefix: &str) -> Option<(String, Rect)> {
        self.shapes.iter().find_map(|c| match &c.shape {
            Shape::Text(t) if t.galley.text().starts_with(prefix) => Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
            _ => None,
        })
    }

    fn at(&self, text: &str) -> Pos2 {
        match self.find(text) {
            Some(r) => r.center(),
            None => panic!("`{text}` is not on screen; drew {} texts", self.texts().len()),
        }
    }

    fn click(&mut self, at: Pos2) {
        for pressed in [true, false] {
            self.frame(vec![
                Event::PointerMoved(at),
                Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
            ]);
        }
        self.frames(2);
    }
}

/// A Models folder that exists nowhere, so a transcription ends at once with "model not found", and
/// a download would land there.
fn no_models(rig: &mut Rig, tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pa-tools-model-download-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    rig.app.transcript.models_dir = dir.to_string_lossy().into_owned();
    dir
}

fn open(rig: &mut Rig) {
    rig.app.run("window.transcript", json!({"value": true})).unwrap();
    rig.frames(3);
}

/// Runs a transcription to the end (no injected recogniser, so the window looks for the model and
/// finds none), leaving the window offering a download.
fn provoke_missing_model(rig: &mut Rig) {
    open(rig);
    let at = rig.at("Transcribe Track");
    rig.click(at);
    let started = Instant::now();
    while rig.app.transcript.is_running() && started.elapsed() < Duration::from_secs(10) {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(10));
    }
    rig.frames(2);
    assert!(!rig.app.transcript.is_running(), "the worker did not finish in time");
}

/// The default model's neutral display name (what the button and status show).
fn model_label() -> &'static str {
    models::label(models::DEFAULT_MODEL)
}

#[test]
fn the_download_button_appears_only_after_the_model_is_found_missing() {
    let mut rig = Rig::new();
    no_models(&mut rig, "offer");
    open(&mut rig);
    // Before any attempt there is no offer and no button.
    assert!(!rig.app.transcript.is_offering_download());
    assert!(rig.find_prefix("Download ").is_none(), "no button yet: {:?}", rig.texts());

    provoke_missing_model(&mut rig);
    assert!(rig.app.transcript.is_offering_download(), "the missing model is offered for download: {}", rig.app.transcript.status);
    let (button, _) = rig.find_prefix(&format!("Download {}", model_label())).expect("the Download button is shown");
    // The button names the model's neutral display name and a size in MB; never the id or maker.
    assert!(button.contains("MB"), "{button}");
    let lower = button.to_lowercase();
    assert!(!lower.contains("whisper") && !lower.contains("openai") && !lower.contains("hugging"), "{button}");
}

/// A downloader that reports one progress tick, then parks until the test releases it (sends on, or
/// drops, the channel). After release it honours cancel — the shape of a download that is still
/// mid-flight when the user acts. Mirrors the `Gated` recogniser in `transcript_window.rs`, so the
/// thread is genuinely blocked inside the downloader at the moment Cancel is pressed.
fn gated_downloader(release: std::sync::mpsc::Receiver<()>) -> Downloader {
    let release = Mutex::new(release);
    Arc::new(move |_dir, m, progress: &mut dyn FnMut(u64, u64, &str) -> bool| {
        let total = m.size();
        // One tick so the bar shows the stage, then block until the test lets go.
        if !progress(total / 3, total, "model.safetensors") {
            return Err(SpeechError::Cancelled);
        }
        let _ = release.lock().map(|r| r.recv());
        if progress(total, total, "model.safetensors") { Ok(()) } else { Err(SpeechError::Cancelled) }
    })
}

#[test]
fn a_download_reports_progress_then_succeeds_and_clears_the_offer() {
    let mut rig = Rig::new();
    no_models(&mut rig, "success");
    let (tx, rx) = std::sync::mpsc::channel();
    rig.app.transcript.inject_downloader(gated_downloader(rx));

    provoke_missing_model(&mut rig);
    let at = rig.at(&rig.find_prefix(&format!("Download {}", model_label())).expect("button").0);
    rig.click(at);
    assert!(rig.app.transcript.is_downloading(), "the download is running");

    // Pump a few frames: the worker's first progress tick lands and the bar shows the stage.
    for _ in 0..6 {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(rig.texts().iter().any(|t| t.contains("Downloading the speech model")), "the bar shows the stage: {:?}", rig.texts());

    // Release it: the download finishes, the offer is withdrawn, and the status says it is ready.
    tx.send(()).unwrap();
    let started = Instant::now();
    while rig.app.transcript.is_downloading() && started.elapsed() < Duration::from_secs(10) {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
    rig.frames(2);
    assert!(!rig.app.transcript.is_downloading(), "the download finished");
    assert!(!rig.app.transcript.is_offering_download(), "the offer is withdrawn once the model is ready");
    let status = rig.app.transcript.status.clone();
    assert!(status.contains(model_label()) && status.contains("ready"), "{status}");
    assert_eq!(rig.app.ui.status, status, "the main status line says it too");
    assert!(!status.to_lowercase().contains("whisper"), "{status}");
    // The Download button is gone.
    assert!(rig.find_prefix("Download ").is_none(), "{:?}", rig.texts());
}

#[test]
fn cancelling_a_download_winds_down_cleanly_then_lets_it_run_again() {
    let mut rig = Rig::new();
    no_models(&mut rig, "cancel");
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    rig.app.transcript.inject_downloader(gated_downloader(rx));

    provoke_missing_model(&mut rig);
    let at = rig.at(&rig.find_prefix(&format!("Download {}", model_label())).expect("button").0);
    rig.click(at);
    assert!(rig.app.transcript.is_downloading());

    rig.frames(2);
    let at = rig.at("Cancel");
    rig.click(at);
    assert!(!rig.app.transcript.is_downloading(), "Cancel forgets the running download");
    assert!(rig.app.transcript.is_download_stopping(), "but its thread is still parked inside the downloader");
    assert_eq!(rig.app.transcript.status, "Stopping the download…");
    assert!(rig.find("Stopping the download…").is_some(), "the window says so: {:?}", rig.texts());

    // Let the parked thread go: it wakes, sees the cancel (progress returns false) and ends; the
    // window reaps it on its own.
    drop(tx);
    let until = Instant::now() + Duration::from_secs(10);
    while rig.app.transcript.is_download_stopping() && Instant::now() < until {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
    rig.frames(3);
    assert!(!rig.app.transcript.is_download_stopping(), "the old thread ended");
    assert_eq!(rig.app.transcript.status, "Download stopped");
}

#[test]
fn a_download_that_fails_shows_a_short_line_and_names_no_third_party() {
    let mut rig = Rig::new();
    no_models(&mut rig, "fail");
    // A downloader that fails the way the network might, with a URL in the detail.
    let downloader: Downloader =
        Arc::new(|_dir, _m, _progress| Err(SpeechError::Download("https://example.invalid/model.safetensors: connection reset".into())));
    rig.app.transcript.inject_downloader(downloader);

    provoke_missing_model(&mut rig);
    let at = rig.at(&rig.find_prefix(&format!("Download {}", model_label())).expect("button").0);
    rig.click(at);
    let started = Instant::now();
    while rig.app.transcript.is_downloading() && started.elapsed() < Duration::from_secs(10) {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
    rig.frames(2);
    assert!(!rig.app.transcript.is_downloading());
    let status = rig.app.transcript.status.clone();
    assert!(status.contains("could not be downloaded") && status.contains("internet"), "{status}");
    let lower = status.to_lowercase();
    assert!(
        !lower.contains("whisper") && !lower.contains("example.invalid") && !lower.contains("http"),
        "the detail stays out of the status: {status}"
    );
    assert_eq!(rig.app.ui.status, status, "the main status line says it too");
    // The offer stands, so the user can try again.
    assert!(rig.app.transcript.is_offering_download());
}

#[test]
fn transcribe_is_disabled_while_a_download_runs() {
    let mut rig = Rig::new();
    no_models(&mut rig, "exclusive");
    // The channel sender is held to the end of the test, so the download stays parked throughout.
    let (_tx, rx) = std::sync::mpsc::channel::<()>();
    rig.app.transcript.inject_downloader(gated_downloader(rx));

    provoke_missing_model(&mut rig);
    let at = rig.at(&rig.find_prefix(&format!("Download {}", model_label())).expect("button").0);
    rig.click(at);
    assert!(rig.app.transcript.is_downloading());

    // The Transcribe button is greyed out: clicking it starts no transcription.
    let at = rig.at("Transcribe Track");
    rig.click(at);
    assert!(!rig.app.transcript.is_running(), "no transcription starts while a download runs");
    assert!(rig.app.transcript.is_downloading(), "the download is untouched");
}
