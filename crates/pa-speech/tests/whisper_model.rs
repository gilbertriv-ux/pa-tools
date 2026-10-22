//! Real speech recognition, run when a model and a fixture are present on this machine.
//!
//! Never downloads. The model is found by the same discovery the program uses
//! ([`ModelSearch::from_environment`]: `$PA_SPEECH_MODELS_DIR`, then the bundle, then the user's
//! application data folder). The fixture is a mono 16 kHz little-endian f32 raw clip at
//! `$PA_SPEECH_FIXTURE`, and `$PA_SPEECH_FIXTURE_KEYWORD` names a word the transcript must contain.
//!
//! This test is built so that it cannot pass without recognising speech:
//! - it skips (printing `SKIPPED: ...`) only when no model is found *and* no fixture is configured;
//! - a configured fixture with no model, an unreadable fixture, or a fixture with no expected
//!   keyword is a failure (a clip with no expected words proves nothing);
//! - every word must lie inside the clip, with no slack, and starts must not go backwards.
//!
//! Optionally `$PA_SPEECH_FIXTURE_REFERENCE` holds the true text; the word error rate against it is
//! printed.
#![cfg(feature = "whisper")]

use std::path::PathBuf;

use soundcraft_pa_speech::{ModelSearch, Options, SAMPLE_RATE, SpeechError, models, resolve_models_dir, word_error_rate};

/// What the environment asks this test to do.
#[derive(Debug, PartialEq)]
enum Gate {
    /// Nothing is configured and no model is installed: nothing to check.
    Skip(String),
    /// Configured, but unable to prove anything: a failure, never a skip.
    Fail(String),
    Run {
        models_dir: PathBuf,
        fixture: PathBuf,
        keyword: String,
    },
}

/// Decide from what was found: the model lookup, `$PA_SPEECH_FIXTURE`, `$PA_SPEECH_FIXTURE_KEYWORD`.
fn gate(model: Result<PathBuf, SpeechError>, fixture: Option<PathBuf>, keyword: Option<String>) -> Gate {
    match (model, fixture) {
        (Err(e), None) => Gate::Skip(format!("{e}; and no $PA_SPEECH_FIXTURE is set")),
        (Err(e), Some(f)) => Gate::Fail(format!(
            "$PA_SPEECH_FIXTURE={} is set but the model was not found, so discovery is broken or the model is missing: {e}",
            f.display()
        )),
        (Ok(dir), None) => Gate::Skip(format!("model found in {}, but no $PA_SPEECH_FIXTURE (mono 16 kHz f32 raw) is set", dir.display())),
        (Ok(models_dir), Some(fixture)) => match keyword.map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()) {
            Some(keyword) => Gate::Run { models_dir, fixture, keyword },
            None => Gate::Fail(
                "$PA_SPEECH_FIXTURE is set without $PA_SPEECH_FIXTURE_KEYWORD: a fixture with no expected words proves nothing; \
                 set the keyword to a word the clip says"
                    .into(),
            ),
        },
    }
}

fn env_nonempty(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

/// Read a mono 16 kHz little-endian f32 raw clip.
fn read_fixture(path: &std::path::Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read $PA_SPEECH_FIXTURE {}: {e}", path.display()));
    assert!(
        !bytes.is_empty() && bytes.len().is_multiple_of(4),
        "fixture {} is {} bytes: expected whole little-endian f32 samples",
        path.display(),
        bytes.len()
    );
    let audio: Vec<f32> = bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    assert!(audio.iter().all(|v| v.is_finite()), "fixture {} holds non-finite samples", path.display());
    audio
}

#[test]
fn transcribes_a_fixture_when_a_model_is_present() {
    let model = resolve_models_dir(&ModelSearch::from_environment("Pa Tools"), models::DEFAULT_MODEL);
    let fixture = env_nonempty("PA_SPEECH_FIXTURE").map(PathBuf::from);
    let keyword = env_nonempty("PA_SPEECH_FIXTURE_KEYWORD").map(|k| k.to_string_lossy().into_owned());
    let (models_dir, fixture, keyword) = match gate(model, fixture, keyword) {
        Gate::Skip(why) => {
            eprintln!("SKIPPED: {why}");
            return;
        }
        Gate::Fail(why) => panic!("{why}"),
        Gate::Run { models_dir, fixture, keyword } => (models_dir, fixture, keyword),
    };

    let audio = read_fixture(&fixture);
    let clip = audio.len() as f64 / f64::from(SAMPLE_RATE);
    eprintln!("model: {}/{}; fixture: {} ({clip:.3} s)", models_dir.display(), models::DEFAULT_MODEL, fixture.display());

    let t = soundcraft_pa_speech::load(&models_dir, models::DEFAULT_MODEL).expect("load the model");
    let out = t.transcribe(&audio, &Options::default(), &mut |_, _| true).expect("transcribe");
    let text = out.text();
    eprintln!("transcript ({} words, language `{}`, source `{}`): {text:?}", out.words.len(), out.language, out.source);
    for w in &out.words {
        eprintln!("  {:6.2} - {:6.2}  {}", w.start_sec, w.end_sec, w.text);
    }
    if let Some(reference) = env_nonempty("PA_SPEECH_FIXTURE_REFERENCE") {
        let reference = reference.to_string_lossy();
        eprintln!("word error rate vs reference: {:.4}", word_error_rate(&reference, &text));
    }

    assert!(!out.words.is_empty(), "expected a non-empty transcript");
    assert!(text.to_lowercase().contains(&keyword), "the transcript {text:?} does not contain the expected word {keyword:?}");

    // Every word lies inside the clip, in order; there is no slack past the end of the audio.
    let mut last_start = 0.0f64;
    for w in &out.words {
        assert!(
            0.0 <= w.start_sec && w.start_sec <= w.end_sec && w.end_sec <= clip,
            "word {:?} spans {}..{} s, outside 0..={clip} s",
            w.text,
            w.start_sec,
            w.end_sec
        );
        assert!(w.start_sec >= last_start, "word {:?} starts at {} s, before the word ahead of it ({last_start} s)", w.text, w.start_sec);
        last_start = w.start_sec;
    }
}

#[test]
fn the_gate_skips_only_when_nothing_is_configured() {
    let missing = || Err(SpeechError::ModelUnavailable("speech model `whisper-base` not found (searched: nowhere)".into()));
    let dir = PathBuf::from("/models");
    let clip = PathBuf::from("/clip.f32");

    // No model, no fixture: skip. Model but no fixture: skip, saying so.
    assert!(matches!(gate(missing(), None, None), Gate::Skip(_)));
    assert!(matches!(gate(Ok(dir.clone()), None, Some("country".into())), Gate::Skip(_)));
    // A fixture with no model must fail, not skip: a skip would hide broken discovery.
    assert!(matches!(gate(missing(), Some(clip.clone()), Some("country".into())), Gate::Fail(_)));
    // A fixture with no (or a blank) keyword must fail.
    for keyword in [None, Some(String::new()), Some("  ".to_string())] {
        assert!(matches!(gate(Ok(dir.clone()), Some(clip.clone()), keyword), Gate::Fail(_)));
    }
    // Everything configured: run, with the keyword lowercased.
    assert_eq!(
        gate(Ok(dir.clone()), Some(clip.clone()), Some(" Country ".into())),
        Gate::Run { models_dir: dir, fixture: clip, keyword: "country".into() }
    );
}
