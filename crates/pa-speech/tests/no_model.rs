//! Always-run tests: no model, no committed audio. They exercise the real length/ordering logic
//! via `FixedTranscriber` on a synthesized silent buffer, and the `load` gate's error path.

use soundcraft_pa_speech::{FixedTranscriber, Options, SAMPLE_RATE, SpeechError, Transcriber, Transcript, Word, load};

/// A silent mono buffer of `secs` seconds at 16 kHz.
fn silence(secs: f64) -> Vec<f32> {
    vec![0.0f32; (SAMPLE_RATE as f64 * secs) as usize]
}

#[test]
fn fixed_transcriber_drops_out_of_range_and_keeps_order() {
    // Clip is 2.0 s. Two words inside, one past the end.
    let audio = silence(2.0);
    let transcript = Transcript {
        words: vec![
            Word::new("hello", 0.10, 0.45),
            Word::new("world", 0.60, 0.95),
            Word::new("late", 5.00, 5.40),       // starts past the clip end -> dropped
            Word::new("straddle", 1.80, 2.40),   // starts inside but ends past the end -> dropped
            Word::new("edge", 1.50, 2.00),       // ends exactly at the end -> kept
            Word::new("broken", 0.20, f64::NAN), // no finite bounds -> dropped
        ],
        language: "en".into(),
        source: "fixture".into(),
    };
    let f = FixedTranscriber { transcript, id: "fixed".into() };
    let out = f.transcribe(&audio, &Options::default(), &mut |_, _| true).expect("transcribe ok");

    // Every word that is not entirely inside the clip is dropped.
    let kept: Vec<&str> = out.words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(kept, ["hello", "world", "edge"], "only words that end inside the clip are kept");
    // `source` is set from the transcriber id.
    assert_eq!(out.source, "fixed");
    // Timestamps are monotonic and within [0, clip_len].
    let clip_len = audio.len() as f64 / SAMPLE_RATE as f64;
    let mut last = 0.0f64;
    for w in &out.words {
        assert!(w.start_sec >= last, "starts must be non-decreasing");
        assert!(w.start_sec <= w.end_sec, "start <= end");
        assert!(w.end_sec <= clip_len + 1e-9, "word must end within the clip");
        last = w.start_sec;
    }
}

#[test]
fn cancelling_progress_stops_transcription() {
    let f = FixedTranscriber { transcript: Transcript::default(), id: "fixed".into() };
    let err = f.transcribe(&silence(1.0), &Options::default(), &mut |_, _| false).unwrap_err();
    assert_eq!(err, SpeechError::Cancelled);
}

#[test]
fn load_without_model_reports_not_installed() {
    // A real catalogue id, but no weights on disk -> NotInstalled (never a silent empty pass).
    let dir = std::env::temp_dir().join(format!("pa-speech-load-{}", std::process::id()));
    // `.err()` drops the Ok value (an `Arc<dyn Transcriber>`, which isn't `Debug`).
    assert_eq!(load(&dir, "whisper-base").err(), Some(SpeechError::NotInstalled("whisper-base".into())));
    // An unknown id is a distinct error.
    assert_eq!(load(&dir, "whisper-nope").err(), Some(SpeechError::UnknownModel("whisper-nope".into())));
}
