//! Speech-to-text (`soundcraft-pa-speech`). One command: render a track's audio, downmix to mono,
//! resample to 16 kHz, run the recogniser, and store the resulting transcript on the session.
//!
//! Actual recognition needs a downloaded Whisper model and a build with the `whisper` feature; the
//! command reports a clear status when either is missing instead of failing.

use super::*;
use crate::cmd;
use serde_json::json;
use soundcraft_model::{TrackKind, Transcript, TranscriptWord};
use soundcraft_time::Range;
use std::path::PathBuf;

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        noundo "track.transcribe",
        "Transcribe Track...",
        ["Track"],
        None,
        "{track?: id|name, model?: 'whisper-base', models_dir?: path, language?: 'en', diarize?: bool} — \
         speech-to-text for a track; the model downloads first via Setup › Transcription Settings",
        always,
        transcribe
    )]
}

fn transcribe(e: &mut Engine, p: &Value) -> Result<Value> {
    let cmd = "track.transcribe";

    // The track: explicit `track`, else the first audio track.
    let track = match track_param(e, cmd, p, "track")? {
        Some(t) => t,
        None => e
            .session()
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio)
            .map(|t| t.id)
            .ok_or_else(|| bad(cmd, "no audio track to transcribe".to_string()))?,
    };

    let model = str_param(p, "model").unwrap_or(soundcraft_pa_speech::models::DEFAULT_MODEL).to_string();
    let models_dir = str_param(p, "models_dir")
        .map(PathBuf::from)
        .or_else(|| soundcraft_pa_speech::discovery::resolve_models_dir(&model, soundcraft_brand::CONFIG_DIR_NAME).ok());

    // Render the track's clips and downmix to mono at the session rate, then resample to 16 kHz.
    let sr = e.session().sample_rate;
    let end = e.session().content_end();
    let channels = soundcraft_mix::render_clips(e.session(), track, Range::new(0, end));
    let mono = soundcraft_pa_speech::downmix(&channels);
    // High-quality (Kaiser-windowed sinc, anti-aliased) rate conversion to the recogniser's 16 kHz.
    let audio = soundcraft_dsp::offline::resample(&[mono], sr.hz(), soundcraft_pa_speech::SAMPLE_RATE).into_iter().next().unwrap_or_default();

    // A model must be on disk (and the `whisper` feature built in) to recognise speech.
    let Some(dir) = models_dir else {
        return Ok(json!({
            "status": "no_models_dir", "track": track.0, "model": model, "samples_16k": audio.len(),
            "hint": "pass models_dir or set PA_SPEECH_MODELS_DIR; download the model via Setup › Transcription Settings"
        }));
    };
    let transcriber = match soundcraft_pa_speech::load(&dir, &model) {
        Ok(t) => t,
        Err(err) => {
            return Ok(json!({
                "status": "model_unavailable", "track": track.0, "model": model, "error": err.to_string(),
                "hint": "download the model (Setup › Transcription Settings), or build with the `whisper` feature"
            }));
        }
    };

    let opts = soundcraft_pa_speech::Options {
        language: str_param(p, "language").map(str::to_string),
        diarize: p.get("diarize").and_then(Value::as_bool).unwrap_or(true),
        max_speakers: 6,
    };
    let produced =
        transcriber.transcribe(&audio, &opts, &mut |_fraction, _status| true).map_err(|err| bad(cmd, format!("transcription failed: {err}")))?;

    // Convert the recogniser's seconds to session samples and store.
    let words: Vec<TranscriptWord> = produced
        .words
        .iter()
        .map(|w| TranscriptWord { start: sr.samples(w.start_sec), end: sr.samples(w.end_sec), text: w.text.clone(), speaker: w.speaker })
        .collect();
    let count = words.len();
    let text = produced.text();
    let (language, source) = (produced.language.clone(), produced.source.clone());
    e.session_mut().set_transcript(Transcript { track, words, language: language.clone(), source: source.clone() });

    Ok(json!({"status": "ok", "track": track.0, "words": count, "language": language, "source": source, "text": text}))
}
