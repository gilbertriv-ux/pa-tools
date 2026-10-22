//! Speech-to-text (`soundcraft-pa-speech`). Render a track's audio, downmix to mono, resample to
//! 16 kHz, run the recogniser, and store the resulting transcript on the session.
//!
//! Actual recognition needs a downloaded Whisper model and a build with the `whisper` feature; the
//! command reports a clear status (`model_unavailable`, with the reason and every place searched)
//! when either is missing instead of failing. Where the model is looked for: an explicit
//! `models_dir` parameter, else [`soundcraft_pa_speech::resolve_models_dir`] (`$PA_SPEECH_MODELS_DIR`,
//! then the app bundle's resources, then the user's application data folder).
//!
//! Two ways in, one code path:
//! - `track.transcribe` does everything on the calling thread (CLI, MCP, scripts).
//! - [`transcribe_job`] gathers the audio on the calling thread and [`TranscribeJob::run`] does the
//!   recognition on any thread, so a UI stays responsive (like [`super::bounce_job`]); the UI then
//!   stores the result with `track.set_transcript`, which agents can also call with a transcript
//!   they bring themselves.

use super::*;
use crate::cmd;
use serde_json::json;
use soundcraft_model::{TrackKind, Transcript, TranscriptWord};
use soundcraft_pa_speech::{ModelSearch, resolve_models_dir};
use soundcraft_time::{Range, SampleRate};
use std::path::PathBuf;

const TRANSCRIBE: &str = "track.transcribe";
const SET_TRANSCRIPT: &str = "track.set_transcript";

/// Most words one stored transcript may hold (about 100 hours of speech); more is refused.
pub const MAX_TRANSCRIPT_WORDS: usize = 2_000_000;
/// Longest word text kept, in bytes; longer text is cut at a character boundary.
const MAX_WORD_BYTES: usize = 256;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            noundo "track.transcribe",
            "Transcribe Track...",
            ["Track"],
            None,
            "{track?: id|name, model?: 'whisper-base', models_dir?: path, language?: 'en', diarize?: bool} — \
             speech-to-text for a track; the model downloads first via Setup › Transcription Settings and is \
             found automatically (`models_dir` overrides the search)",
            always,
            transcribe
        ),
        cmd!(
            noundo "track.set_transcript",
            "Set Track Transcript",
            [],
            None,
            "{track: id|name, words: [{start, end, text, speaker?}], language?, source?} — stores a transcript, \
             replacing the track's; `start`/`end` are samples from session start; `words: []` stores an empty one",
            always,
            set_transcript
        ),
    ]
}

/// `track.transcribe`'s inputs, gathered on the calling thread: the track's audio as mono 16 kHz
/// and where the model is (or why it was not found). [`TranscribeJob::run`] does the (slow)
/// recognition and may run on any thread; it never touches the session.
pub struct TranscribeJob {
    /// The track being transcribed.
    pub track: TrackId,
    sample_rate: SampleRate,
    audio: Vec<f32>,
    model: String,
    /// The folder holding the model, or why none was found.
    models_dir: std::result::Result<PathBuf, soundcraft_pa_speech::SpeechError>,
    opts: soundcraft_pa_speech::Options,
}

/// What a [`TranscribeJob`] produced: `track.transcribe`'s result value, and the transcript to
/// store (present only when the result's `status` is `ok`).
#[derive(Debug)]
pub struct TranscribeOutcome {
    pub result: Value,
    pub transcript: Option<Transcript>,
}

/// Gather what `track.transcribe` needs for `p`, without recognising anything yet. The model is
/// looked for where the running program would find it ([`ModelSearch::from_environment`]).
pub fn transcribe_job(e: &Engine, p: &Value) -> Result<TranscribeJob> {
    transcribe_job_with(e, p, &ModelSearch::from_environment(soundcraft_brand::APP_NAME))
}

/// [`transcribe_job`] with the places to look for the model given (tests pass their own, so no
/// process environment is touched).
fn transcribe_job_with(e: &Engine, p: &Value, search: &ModelSearch) -> Result<TranscribeJob> {
    // The track: explicit `track`, else the first audio track.
    let track = match track_param(e, TRANSCRIBE, p, "track")? {
        Some(t) => t,
        None => e
            .session()
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio)
            .map(|t| t.id)
            .ok_or_else(|| bad(TRANSCRIBE, "no audio track to transcribe".to_string()))?,
    };

    let model = str_param(p, "model").unwrap_or(soundcraft_pa_speech::models::DEFAULT_MODEL).to_string();
    // An explicit folder wins (and is not second-guessed: if the model is not in it, that is the
    // answer). Otherwise search.
    let models_dir = match str_param(p, "models_dir") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => resolve_models_dir(search, &model),
    };

    // Render the track's clips and downmix to mono at the session rate, then resample to 16 kHz.
    let sr = e.session().sample_rate;
    let end = e.session().content_end();
    let channels = soundcraft_mix::render_clips(e.session(), track, Range::new(0, end));
    let mono = soundcraft_pa_speech::downmix(&channels);
    // High-quality (Kaiser-windowed sinc, anti-aliased) rate conversion to the recogniser's 16 kHz.
    let audio = soundcraft_dsp::offline::resample(&[mono], sr.hz(), soundcraft_pa_speech::SAMPLE_RATE).into_iter().next().unwrap_or_default();

    let opts = soundcraft_pa_speech::Options {
        language: str_param(p, "language").map(str::to_string),
        diarize: p.get("diarize").and_then(Value::as_bool).unwrap_or(true),
        max_speakers: 6,
    };
    Ok(TranscribeJob { track, sample_rate: sr, audio, model, models_dir, opts })
}

impl TranscribeJob {
    /// Length of the audio to recognise, in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.audio.len() as f64 / f64::from(soundcraft_pa_speech::SAMPLE_RATE)
    }

    /// Recognise the speech. `progress(fraction, status)` is called as it goes; returning `false`
    /// stops it (an error that says so). A model that cannot be found or loaded is not an error:
    /// the outcome's `status` is `model_unavailable`, its `error` says why (and lists the places
    /// searched), and it carries no transcript.
    pub fn run(self, progress: soundcraft_pa_speech::ProgressFn) -> Result<TranscribeOutcome> {
        let TranscribeJob { track, sample_rate: sr, audio, model, models_dir, opts } = self;

        // A model must be on disk (and the `whisper` feature built in) to recognise speech.
        let transcriber = match models_dir.and_then(|dir| soundcraft_pa_speech::load(&dir, &model)) {
            Ok(t) => t,
            Err(err) => {
                let result = json!({
                    "status": "model_unavailable", "track": track.0, "model": model, "error": err.to_string(),
                    "samples_16k": audio.len(),
                    "hint": "download the model (Setup › Transcription Settings), pass models_dir or set PA_SPEECH_MODELS_DIR, \
                             or build with the `whisper` feature"
                });
                return Ok(TranscribeOutcome { result, transcript: None });
            }
        };

        let produced = transcriber.transcribe(&audio, &opts, progress).map_err(|err| bad(TRANSCRIBE, format!("transcription failed: {err}")))?;

        // Convert the recogniser's seconds to session samples (origin: session start).
        let words: Vec<TranscriptWord> = produced
            .words
            .iter()
            .map(|w| TranscriptWord { start: sr.samples(w.start_sec), end: sr.samples(w.end_sec), text: w.text.clone(), speaker: w.speaker })
            .collect();
        let count = words.len();
        let text = produced.text();
        let (language, source) = (produced.language.clone(), produced.source.clone());
        let result = json!({"status": "ok", "track": track.0, "words": count, "language": language, "source": source, "text": text});
        Ok(TranscribeOutcome { result, transcript: Some(Transcript { track, words, language, source }) })
    }
}

fn transcribe(e: &mut Engine, p: &Value) -> Result<Value> {
    let outcome = transcribe_job(e, p)?.run(&mut |_fraction, _status| true)?;
    if let Some(t) = outcome.transcript {
        e.session_mut().set_transcript(t);
    }
    Ok(outcome.result)
}

/// `s` cut to at most `max` bytes, at a character boundary.
fn clip_text(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

fn set_transcript(e: &mut Engine, p: &Value) -> Result<Value> {
    let track = track_param(e, SET_TRANSCRIPT, p, "track")?.ok_or_else(|| bad(SET_TRANSCRIPT, "`track` required"))?;
    let list = p.get("words").and_then(Value::as_array).ok_or_else(|| bad(SET_TRANSCRIPT, "`words` must be an array"))?;
    if list.len() > MAX_TRANSCRIPT_WORDS {
        return Err(bad(SET_TRANSCRIPT, format!("at most {MAX_TRANSCRIPT_WORDS} words")));
    }
    let mut words = Vec::with_capacity(list.len());
    for (i, w) in list.iter().enumerate() {
        let sample = |key: &str| {
            w.get(key).and_then(Value::as_i64).ok_or_else(|| bad(SET_TRANSCRIPT, format!("words[{i}].{key} must be a whole number of samples")))
        };
        let (start, end) = (sample("start")?, sample("end")?);
        if start < 0 || end < start {
            return Err(bad(SET_TRANSCRIPT, format!("words[{i}]: need 0 <= start <= end (got {start}..{end})")));
        }
        let text = w.get("text").and_then(Value::as_str).ok_or_else(|| bad(SET_TRANSCRIPT, format!("words[{i}].text must be a string")))?;
        let speaker = w.get("speaker").and_then(Value::as_u64).map_or(0, |s| u8::try_from(s).unwrap_or(u8::MAX));
        words.push(TranscriptWord {
            start: start.min(MAX_POSITION),
            end: end.min(MAX_POSITION),
            text: clip_text(text.trim(), MAX_WORD_BYTES).to_string(),
            speaker,
        });
    }
    // Stored transcripts are in time order (stable, so equal starts keep their given order).
    words.sort_by_key(|w| (w.start, w.end));
    let language: String = str_param(p, "language").unwrap_or("").chars().take(16).collect();
    let source: String = str_param(p, "source").unwrap_or("").chars().take(64).collect();
    let count = words.len();
    e.session_mut().set_transcript(Transcript { track, words, language, source });
    Ok(json!({"track": track.0, "words": count}))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::demo::demo_engine;

    fn first_audio(e: &Engine) -> TrackId {
        e.session().tracks.iter().find(|t| t.kind == TrackKind::Audio).map(|t| t.id).unwrap()
    }

    #[test]
    fn set_transcript_stores_sorted_words_and_replaces() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let words = json!([
            {"start": 48_000, "end": 60_000, "text": " world ", "speaker": 1},
            {"start": 0, "end": 12_000, "text": "hello", "speaker": 300},
        ]);
        let r = e.execute(SET_TRANSCRIPT, &json!({"track": t.0, "words": words, "language": "en", "source": "test"})).unwrap();
        assert_eq!(r["words"], 2);
        let got = e.session().transcript(t).unwrap();
        assert_eq!(got.text(), "hello world");
        assert_eq!(got.words[0].speaker, u8::MAX, "speaker ids past 255 saturate");
        assert_eq!(got.words[1].speaker, 1);
        assert_eq!((got.language.as_str(), got.source.as_str()), ("en", "test"));
        assert!(e.is_dirty());

        // Storing again replaces, never duplicates.
        e.execute(SET_TRANSCRIPT, &json!({"track": t.0, "words": []})).unwrap();
        assert_eq!(e.session().transcripts.iter().filter(|x| x.track == t).count(), 1);
        assert!(e.session().transcript(t).unwrap().words.is_empty());
    }

    #[test]
    fn set_transcript_refuses_bad_input_and_leaves_the_session_alone() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let before = e.session().transcripts.clone();
        for bad_params in [
            json!({"words": []}),
            json!({"track": 999_999, "words": []}),
            json!({"track": t.0}),
            json!({"track": t.0, "words": [{"start": 10, "end": 5, "text": "x"}]}),
            json!({"track": t.0, "words": [{"start": -1, "end": 5, "text": "x"}]}),
            json!({"track": t.0, "words": [{"start": 1.5, "end": 5, "text": "x"}]}),
            json!({"track": t.0, "words": [{"start": 0, "end": 5}]}),
            json!({"track": t.0, "words": [{"start": 0, "end": u64::MAX, "text": "x"}]}),
        ] {
            assert!(e.execute(SET_TRANSCRIPT, &bad_params).is_err(), "{bad_params}");
        }
        assert_eq!(e.session().transcripts, before);
    }

    #[test]
    fn long_word_text_is_cut_at_a_character_boundary() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let long = "é".repeat(MAX_WORD_BYTES); // 2 bytes each: twice the limit
        e.execute(SET_TRANSCRIPT, &json!({"track": t.0, "words": [{"start": 0, "end": 1, "text": long}]})).unwrap();
        let w = &e.session().transcript(t).unwrap().words[0];
        assert!(w.text.len() <= MAX_WORD_BYTES && !w.text.is_empty());
        assert!(w.text.chars().all(|c| c == 'é'));
    }

    /// A scratch folder removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("pa-tools-transcribe-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Install the default model under `models_dir` as sparse files of the catalogue sizes.
    fn fake_install(models_dir: &std::path::Path) {
        let m = soundcraft_pa_speech::models::find(soundcraft_pa_speech::models::DEFAULT_MODEL).unwrap();
        let dir = soundcraft_pa_speech::models::model_dir(models_dir, m);
        std::fs::create_dir_all(&dir).unwrap();
        for f in m.files {
            std::fs::File::create(dir.join(f.name)).unwrap().set_len(f.size).unwrap();
        }
    }

    #[test]
    fn job_with_no_model_anywhere_is_model_unavailable_and_lists_where_it_looked() {
        let e = demo_engine();
        let t = first_audio(&e);
        let scratch = Scratch::new("nowhere");
        let search = ModelSearch { env: Some(scratch.0.join("env")), bundle_resources: None, app_support: Some(scratch.0.join("support")) };
        let job = transcribe_job_with(&e, &json!({"track": t.0}), &search).unwrap();
        assert_eq!(job.track, t);
        assert!(job.duration_secs() > 0.0, "the demo track has audio to recognise");
        let out = job.run(&mut |_, _| true).unwrap();
        assert_eq!(out.result["status"], "model_unavailable");
        let error = out.result["error"].as_str().unwrap();
        assert!(error.contains("not found (searched:") && error.contains("whisper-base"), "{error}");
        assert!(
            error.contains(&*scratch.0.join("env").to_string_lossy()) && error.contains(&*scratch.0.join("support").to_string_lossy()),
            "{error}"
        );
        assert!(out.result["samples_16k"].as_u64().unwrap() > 0);
        assert!(out.transcript.is_none());
        // The old status for "no folder to look in" is gone: every miss reads the same way.
        let none = transcribe_job_with(&e, &json!({"track": t.0}), &ModelSearch::default()).unwrap().run(&mut |_, _| true).unwrap();
        assert_eq!(none.result["status"], "model_unavailable");
    }

    #[test]
    fn job_finds_a_model_in_the_application_data_folder() {
        let e = demo_engine();
        let t = first_audio(&e);
        let scratch = Scratch::new("found");
        let support = scratch.0.join("support");
        fake_install(&support);
        let search = ModelSearch { env: None, bundle_resources: None, app_support: Some(support) };
        let out = transcribe_job_with(&e, &json!({"track": t.0}), &search).unwrap().run(&mut |_, _| true).unwrap();
        // Discovery succeeded, so the failure (if any) is the loader's own: this build has no
        // recogniser, or the sparse placeholder weights do not parse. It is not "not found".
        assert_eq!(out.result["status"], "model_unavailable");
        let error = out.result["error"].as_str().unwrap();
        assert!(!error.contains("not found (searched"), "{error}");
        assert!(out.transcript.is_none());
    }

    #[test]
    fn explicit_models_dir_wins_over_the_search() {
        let e = demo_engine();
        let t = first_audio(&e);
        let scratch = Scratch::new("explicit");
        let installed = scratch.0.join("installed");
        fake_install(&installed);
        // The search would find a model; the explicit folder is empty and is not second-guessed.
        let search = ModelSearch { env: Some(installed), bundle_resources: None, app_support: None };
        let empty = scratch.0.join("empty");
        let out =
            transcribe_job_with(&e, &json!({"track": t.0, "models_dir": empty.to_string_lossy()}), &search).unwrap().run(&mut |_, _| true).unwrap();
        assert_eq!(out.result["status"], "model_unavailable");
        assert_eq!(out.result["error"], "the speech model `whisper-base` is not downloaded");
    }

    #[test]
    fn model_folders_are_named_from_the_brand() {
        let home = |name: &str| (name == "HOME").then(|| std::ffi::OsString::from("/home/gil"));
        let mac = ModelSearch::from_parts(soundcraft_pa_speech::Platform::Mac, soundcraft_brand::APP_NAME, home, None);
        let expected = std::path::Path::new("/home/gil/Library/Application Support").join(soundcraft_brand::CONFIG_DIR_NAME).join("models");
        assert_eq!(mac.app_support, Some(expected));
        // On the other Unixes the folder is the brand's XDG name, the same one the settings use.
        let other = ModelSearch::from_parts(soundcraft_pa_speech::Platform::Other, soundcraft_brand::APP_NAME, home, None);
        let expected = std::path::Path::new("/home/gil/.local/share").join(soundcraft_brand::CONFIG_DIR_XDG).join("models");
        assert_eq!(other.app_support, Some(expected));
    }

    #[test]
    fn transcribe_command_matches_the_job_path() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let dir = std::env::temp_dir().join(format!("pa-tools-no-models-cmd-{}", std::process::id()));
        let r = e.execute(TRANSCRIBE, &json!({"track": t.0, "models_dir": dir.to_string_lossy()})).unwrap();
        assert_eq!(r["status"], "model_unavailable");
        assert!(e.session().transcript(t).is_none());
    }
}
