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

/// Every message `load` and discovery can hand to a status line names the model by its catalogue
/// display name and leaves out the model's id (a folder name) and any folder it searched.
#[test]
fn load_and_discovery_messages_name_no_third_party_product() {
    use soundcraft_pa_speech::{ModelSearch, models, resolve_models_dir};

    let check = |what: &str, e: &SpeechError| {
        for text in [e.to_string(), e.summary()] {
            let lower = text.to_lowercase();
            assert!(!["whisper", "openai", "hugging"].iter().any(|n| lower.contains(n)), "{what}: {text:?}");
        }
        assert!(!e.summary().contains('\n'), "{what}: the summary is one line");
    };

    // Not installed.
    let dir = std::env::temp_dir().join(format!("pa-speech-names-{}", std::process::id()));
    let not_installed = load(&dir, models::DEFAULT_MODEL).err().expect("nothing installed");
    assert_eq!(not_installed, SpeechError::NotInstalled(models::DEFAULT_MODEL.into()));
    check("not installed", &not_installed);
    assert_eq!(not_installed.to_string(), "Speech model, base (multilingual) is not installed");

    // Not found anywhere (the folders searched are in the full text only).
    let search = ModelSearch { env: Some(dir.join("env")), bundle_resources: None, app_support: Some(dir.join("support")) };
    let not_found = resolve_models_dir(&search, models::DEFAULT_MODEL).expect_err("nothing installed");
    check("not found", &not_found);
    assert!(not_found.to_string().contains("env") && !not_found.summary().contains("support"), "{not_found}");

    // Present on disk but not loadable: without the speech feature "unavailable", with it the
    // placeholder files are rejected. Either way the text is clean.
    let m = models::find(models::DEFAULT_MODEL).expect("in the catalogue");
    let installed = dir.join("installed");
    let model_dir = models::model_dir(&installed, m);
    std::fs::create_dir_all(&model_dir).expect("create model folder");
    for f in m.files {
        std::fs::File::create(model_dir.join(f.name)).and_then(|file| file.set_len(f.size)).expect("sparse file");
    }
    let unloadable = load(&installed, models::DEFAULT_MODEL).err().expect("placeholder files cannot load");
    check("unloadable", &unloadable);
    assert_eq!(matches!(unloadable, SpeechError::Unavailable(_)), !soundcraft_pa_speech::available(), "{unloadable}");
    let _ = std::fs::remove_dir_all(&dir);
}
