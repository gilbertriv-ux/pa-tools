//! Real Whisper transcription, only when a model and a fixture are present on this machine.
//!
//! Never downloads. Looks for the default model under `$PA_SPEECH_MODELS_DIR/<id>/` and a mono
//! 16 kHz little-endian f32 raw clip at `$PA_SPEECH_FIXTURE`. Without either, the test prints
//! SKIPPED and passes, so a model-less CI still goes green.
#![cfg(feature = "whisper")]

use std::path::PathBuf;

use soundcraft_pa_speech::{Options, models};

fn model_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("PA_SPEECH_MODELS_DIR").map(PathBuf::from)?;
    let m = models::find(models::DEFAULT_MODEL)?;
    models::installed(&dir, m).then_some(dir)
}

/// Read a mono 16 kHz little-endian f32 raw clip from `$PA_SPEECH_FIXTURE`.
fn fixture() -> Option<Vec<f32>> {
    let p = std::env::var_os("PA_SPEECH_FIXTURE")?;
    let bytes = std::fs::read(p).ok()?;
    Some(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

#[test]
fn transcribes_a_fixture_when_a_model_is_present() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIPPED: no `{}` under $PA_SPEECH_MODELS_DIR", models::DEFAULT_MODEL);
        return;
    };
    let Some(audio) = fixture() else {
        eprintln!("SKIPPED: no $PA_SPEECH_FIXTURE (mono 16 kHz f32 raw)");
        return;
    };
    let t = soundcraft_pa_speech::load(&dir, models::DEFAULT_MODEL).expect("load the model");
    let out = t.transcribe(&audio, &Options::default(), &mut |_, _| true).expect("transcribe");
    assert!(!out.words.is_empty(), "expected a non-empty transcript");
    // word timestamps are monotonic and within the clip
    let clip = audio.len() as f64 / soundcraft_pa_speech::SAMPLE_RATE as f64;
    let mut last = 0.0f64;
    for w in &out.words {
        assert!(w.start_sec >= last - 1e-6 && w.start_sec <= w.end_sec && w.end_sec <= clip + 1.0);
        last = w.start_sec;
    }
    eprintln!("transcribed {} words: {:?}", out.words.len(), out.text());
}
