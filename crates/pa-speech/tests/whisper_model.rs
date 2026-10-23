//! Real speech recognition, run when a speech fixture is configured.
//!
//! Never downloads. The model is found by the same discovery the program uses
//! ([`ModelSearch::from_environment`]: `$PA_SPEECH_MODELS_DIR`, then the bundle, then the user's
//! application data folder). The fixture is a mono 16 kHz little-endian f32 raw clip at
//! `$PA_SPEECH_FIXTURE`, and `$PA_SPEECH_FIXTURE_KEYWORD` names a word the transcript must contain.
//!
//! When it skips, and when it cannot:
//! - It **skips exactly when `$PA_SPEECH_FIXTURE` is unset** (whether or not a model is found
//!   on this machine): the reason is printed as `SKIPPED: ...` to both stdout and stderr. A skip
//!   proves nothing about speech recognition. `cargo test` hides the output of a passing test, so
//!   run with `-- --nocapture` to see the reason.
//! - With the fixture set it never skips. No model found, an unreadable fixture, or a fixture with
//!   no expected keyword is a failure (a clip with no expected words proves nothing).
//! - When it runs, the transcript must contain the keyword as a **whole word** (compared
//!   case-insensitively, punctuation stripped; `coun` does not match `country`), every word must
//!   lie inside the clip, and the words must be well formed: each ends at or after its start, none
//!   starts before the word ahead of it starts, none overlaps it.
//!
//! Optionally `$PA_SPEECH_FIXTURE_REFERENCE` holds the true text: the word error rate against it is
//! printed and must be below 0.25 (the donor's bar).
#![cfg(feature = "whisper")]

use std::path::PathBuf;

use soundcraft_pa_speech::{ModelSearch, Options, SAMPLE_RATE, SpeechError, models, resolve_models_dir, word_error_rate};

/// The word error rate the transcript may not reach when a reference text is given.
const MAX_WER: f64 = 0.25;

/// What the environment asks this test to do.
#[derive(Debug, PartialEq)]
enum Gate {
    /// No `$PA_SPEECH_FIXTURE`: nothing to check (with or without a model on this machine).
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

/// Lowercase alphanumeric tokens of `text`: everything else (spaces, punctuation) separates them.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_lowercase).collect()
}

/// Whether `text` contains `keyword` as whole words (case-insensitive, punctuation-stripped). A
/// keyword with no word characters never matches.
fn contains_word(text: &str, keyword: &str) -> bool {
    let (text, keyword) = (tokens(text), tokens(keyword));
    !keyword.is_empty() && text.windows(keyword.len()).any(|w| w == keyword.as_slice())
}

/// The first problem with the word timing, if any: inside `0..=clip`, ending no earlier than
/// starting, starting no earlier than the word ahead of it starts, not overlapping it.
fn timing_problem(words: &[soundcraft_pa_speech::Word], clip: f64) -> Option<String> {
    let mut previous: Option<&soundcraft_pa_speech::Word> = None;
    for w in words {
        if !(0.0 <= w.start_sec && w.start_sec <= w.end_sec && w.end_sec <= clip) {
            return Some(format!("word {:?} spans {}..{} s, outside 0..={clip} s or ends before it starts", w.text, w.start_sec, w.end_sec));
        }
        if let Some(p) = previous {
            if w.start_sec < p.start_sec {
                return Some(format!(
                    "word {:?} starts at {} s, before the word ahead of it ({:?} at {} s)",
                    w.text, w.start_sec, p.text, p.start_sec
                ));
            }
            if w.start_sec < p.end_sec {
                return Some(format!(
                    "word {:?} starts at {} s, inside the word ahead of it ({:?} ends at {} s)",
                    w.text, w.start_sec, p.text, p.end_sec
                ));
            }
        }
        previous = Some(w);
    }
    None
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
    let (samples, _) = bytes.as_chunks::<4>();
    let audio: Vec<f32> = samples.iter().map(|c| f32::from_le_bytes(*c)).collect();
    assert!(audio.iter().all(|v| v.is_finite()), "fixture {} holds non-finite samples", path.display());
    audio
}

#[test]
fn transcribes_a_fixture_when_a_model_is_present() {
    let model = resolve_models_dir(&ModelSearch::from_environment("Pa Tools", "patools"), models::DEFAULT_MODEL);
    let fixture = env_nonempty("PA_SPEECH_FIXTURE").map(PathBuf::from);
    let keyword = env_nonempty("PA_SPEECH_FIXTURE_KEYWORD").map(|k| k.to_string_lossy().into_owned());
    let (models_dir, fixture, keyword) = match gate(model, fixture, keyword) {
        Gate::Skip(why) => {
            // Both streams, so it shows in whichever one a runner keeps (hidden unless --nocapture).
            println!("SKIPPED: {why}");
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
    let wer = env_nonempty("PA_SPEECH_FIXTURE_REFERENCE").map(|r| word_error_rate(&r.to_string_lossy(), &text));
    if let Some(wer) = wer {
        eprintln!("word error rate vs reference: {wer:.4} (must be below {MAX_WER})");
    }

    assert!(!out.words.is_empty(), "expected a non-empty transcript");
    assert!(contains_word(&text, &keyword), "the transcript {text:?} does not contain the expected word {keyword:?} as a whole word");
    if let Some(problem) = timing_problem(&out.words, clip) {
        panic!("{problem}");
    }
    if let Some(wer) = wer {
        assert!(wer < MAX_WER, "word error rate {wer:.4} against the reference is not below {MAX_WER}; the transcript was {text:?}");
    }
}

#[test]
fn the_gate_skips_only_without_a_fixture() {
    let missing = || Err(SpeechError::ModelUnavailable { model: "whisper-base".into(), searched: "nowhere".into() });
    let dir = PathBuf::from("/models");
    let clip = PathBuf::from("/clip.f32");

    // No fixture: skip, with or without a model on the machine, saying which.
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

#[test]
fn the_keyword_must_be_a_whole_word() {
    let text = "And so, my fellow Americans, ask not what your country can do for you, ask what you can do for your COUNTRY.";
    assert!(contains_word(text, "country"));
    assert!(contains_word(text, "Country"));
    assert!(contains_word(text, " americans, "));
    assert!(contains_word(text, "my fellow"), "a phrase of whole words matches in order");
    // pieces of words are not words
    for piece in ["coun", "count", "untry", "ountr", "America", "ask n", "fellowAmericans", "for you what"] {
        assert!(!contains_word(text, piece), "{piece:?} must not match");
    }
    // nothing to look for, or nothing to look in
    assert!(!contains_word(text, "") && !contains_word(text, " ,. "));
    assert!(!contains_word("", "country"));
    // punctuation inside a transcript word separates, it does not hide, a keyword
    assert!(contains_word("well-known", "known"));
}

#[test]
fn timing_is_checked_for_order_overlap_and_the_clip() {
    use soundcraft_pa_speech::Word;
    let w = |t: &str, a: f64, b: f64| Word::new(t, a, b);
    let ok = [w("a", 0.0, 0.5), w("b", 0.5, 1.0), w("c", 1.2, 2.0)];
    assert_eq!(timing_problem(&ok, 2.0), None);
    assert_eq!(timing_problem(&[], 2.0), None);
    assert!(timing_problem(&ok, 1.9).is_some(), "ends past the clip");
    assert!(timing_problem(&[w("a", 1.0, 0.5)], 2.0).is_some(), "ends before it starts");
    assert!(timing_problem(&[w("a", -0.1, 0.5)], 2.0).is_some(), "starts before the clip");
    assert!(timing_problem(&[w("a", 0.5, 1.0), w("b", 0.2, 0.4)], 2.0).is_some(), "starts before the word ahead of it starts");
    assert!(timing_problem(&[w("a", 0.0, 1.0), w("b", 0.9, 1.5)], 2.0).is_some(), "overlaps the word ahead of it");
    assert!(timing_problem(&[w("a", 0.0, f64::NAN)], 2.0).is_some(), "not a number");
}
