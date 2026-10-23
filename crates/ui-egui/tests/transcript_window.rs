use egui::vec2;
use egui_kittest::{Harness, kittest::Queryable as _};
use serde_json::json;
use soundcraft_model::{TrackKind, Transcript, TranscriptWord};
use soundcraft_ui_egui::{Services, SoundApp, transcript_window::format_position};

fn harness() -> Harness<'static, SoundApp> {
    let mut engine = soundcraft_engine::demo::demo_engine();
    let track = engine.session().tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().id;
    engine.session_mut().set_transcript(Transcript {
        track,
        language: "en".into(),
        source: "fixture".into(),
        words: vec![
            TranscriptWord { start: 24_000, end: 36_000, text: "hello".into(), speaker: 0 },
            TranscriptWord { start: 48_000, end: 72_000, text: "world".into(), speaker: 1 },
        ],
    });
    let mut app = SoundApp::new(engine, None, Services::default());
    app.run("window.transcript", json!({"value":true})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut SoundApp| {
            app.logic(&ui.ctx().clone());
            soundcraft_ui_egui::transcript_window::show(app, &ui.ctx().clone());
        },
        app,
    );
    h.run_steps(8);
    h
}

#[test]
fn words_seek_to_their_stored_session_samples_and_search_filters() {
    let mut h = harness();
    h.get_by_label("world").click();
    h.run_steps(8);
    assert_eq!(h.state().engine.session().edit.selection.start, 48_000);
    h.state_mut().ui.transcript.search = "HELLO".into();
    h.run_steps(8);
    h.get_by_label("hello");
    assert!(h.query_by_label("world").is_none());
    h.state_mut().run("window.hide_floating", json!({})).unwrap();
    assert!(!h.state().ui.show_transcript);
}

#[test]
fn millisecond_timecodes_round_carry_and_do_not_wrap_at_an_hour() {
    assert_eq!(format_position(24_000, 48_000), "00:00.500");
    assert_eq!(format_position(48_000 * 3600, 48_000), "01:00:00.000");
    assert_eq!(format_position(48_000 * 60 - 1, 48_000), "01:00.000");
    assert_eq!(format_position(-1, 48_000), "00:00.000");
}

#[cfg(all(feature = "whisper", not(target_arch = "wasm32")))]
#[test]
fn transcribe_button_populates_the_window_from_real_speech() {
    let (Some(_), Some(wav)) = (std::env::var_os("PA_SPEECH_MODELS_DIR"), std::env::var_os("PA_SPEECH_WAV_FIXTURE")) else {
        eprintln!("SKIPPED: no real-speech inputs for the Transcript button round-trip");
        return;
    };
    let mut engine = soundcraft_engine::Engine::default();
    engine.execute("file.import_audio", &json!({"path": std::path::PathBuf::from(wav)})).unwrap();
    let track = engine.session().tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().id;
    let mut app = SoundApp::new(engine, None, Services::default());
    app.run("window.transcript", json!({"value":true})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut SoundApp| {
            app.logic(&ui.ctx().clone());
            soundcraft_ui_egui::transcript_window::show(app, &ui.ctx().clone());
        },
        app,
    );
    h.run_steps(8);
    h.get_by_label("Transcribe Track").click();
    h.run_steps(2);
    assert!(h.state().transcription.is_some(), "recognition should run in the background");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while h.state().transcription.is_some() {
        assert!(std::time::Instant::now() < deadline, "transcription timed out");
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.run_steps(1);
    }
    let transcript = h.state().engine.session().transcript(track).expect("button stored the transcript");
    assert!(transcript.text().to_lowercase().contains("transcription"), "{}", transcript.text());
    h.run_steps(4);
    h.get_by_label("transcription");
}
