//! Speech-to-text (`soundcraft-pa-speech`). Render a track's audio, downmix to mono, resample to
//! 16 kHz, run the recogniser, and store the resulting transcript on the session.
//!
//! Actual recognition needs the speech model on disk and a build with the speech feature (the
//! `whisper` cargo feature); the command reports a clear status (`model_unavailable`, with the
//! reason and every place searched) when either is missing instead of failing. Where the model is
//! looked for: an explicit `models_dir` parameter, else [`soundcraft_pa_speech::resolve_models_dir`]
//! (`$PA_SPEECH_MODELS_DIR`, then the app bundle's resources, then the user's application data
//! folder). Nothing downloads the model yet: it has to be put in one of those places.
//!
//! Two ways in, one code path:
//! - `track.transcribe` does everything on the calling thread (CLI, MCP, control channel, scripts):
//!   the call returns when the transcript is stored, and may take minutes for a long track.
//! - [`transcribe_job`] reads the session on the calling thread (the render, which is quick: a
//!   fraction of a second for ten minutes) and [`TranscribeJob::run`] does the rest on any thread,
//!   so a UI stays responsive (like [`super::bounce_job`]): the sample-rate conversion, which takes
//!   tens of seconds for an hour of audio, and the recognition. The UI then stores the result with
//!   `track.set_transcript`, which agents can also call with a transcript they bring themselves.
//!   The app's own menu entry and command palette take this way (the Transcript window's
//!   background job), never the synchronous command.
//!
//! The audio: the track's clips are rendered in ranges of [`RENDER_CHUNK_SECS`] and downmixed to
//! one mono buffer at the session rate, which is what the job carries; [`TranscribeJob::run`]
//! resamples it once to the recogniser's 16 kHz. Rendering a long track in one call would run into
//! `soundcraft_mix::MAX_RENDER_SAMPLES` (2^29 samples summed over channels, which `render_clips`
//! answers by silently cutting the render short); ranges that small never do. Each range is
//! rendered after a run-in that settles the clip effects (see `soundcraft_mix::render_clips_settled`),
//! so a range boundary leaves no mark on the audio. The mono buffer itself is capped at
//! [`MAX_MONO_SAMPLES`], and a track that does not fit is refused before anything is rendered. A
//! transcription never reports `ok` for less than the whole track.
//!
//! Which recogniser runs: [`transcribe_job`] always looks for the speech model on disk when the job
//! runs. [`transcribe_job_with_transcriber`] is the one other way in, for a caller that brings its
//! own [`soundcraft_pa_speech::Transcriber`] (tests; a program that embeds its own recogniser); it
//! has its own name so that nothing can skip the search by accident.

use super::*;
use crate::cmd;
use serde_json::json;
use soundcraft_model::{Session, TrackKind, Transcript, TranscriptWord};
use soundcraft_pa_speech::{ModelSearch, SpeechError, resolve_models_dir};
use soundcraft_time::{Range, SampleRate, Samples};
use std::path::PathBuf;
use std::sync::Arc;

const TRANSCRIBE: &str = "track.transcribe";
const SET_TRANSCRIPT: &str = "track.set_transcript";

/// Longest stretch of a track, in seconds, that is rendered for transcription: the recogniser's own
/// limit ([`soundcraft_pa_speech::MAX_AUDIO_SECS`], 4 hours). Checked before any audio is rendered
/// or allocated, so a very long session is refused at once with a clear error.
pub const MAX_TRANSCRIBE_SECS: usize = soundcraft_pa_speech::MAX_AUDIO_SECS;

/// Most samples the mono, session-rate buffer of one transcription may hold: 2^29 samples, 2 GiB of
/// `f32`, about 3.1 hours at 48 kHz (the resampled 16 kHz audio is a third of that again). A track
/// that is longer at its session rate is refused before it is rendered, with an error that says
/// how long a track may be at that rate.
pub const MAX_MONO_SAMPLES: usize = 1 << 29;

/// Seconds of a track rendered in one call while its audio is gathered.
pub const RENDER_CHUNK_SECS: usize = 60;

/// Most words one stored transcript may hold (about 100 hours of speech); more is refused.
pub const MAX_TRANSCRIPT_WORDS: usize = 2_000_000;
/// Longest word text kept, in bytes; longer text is cut at a character boundary.
const MAX_WORD_BYTES: usize = 256;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "track.transcribe",
            "Transcribe Track",
            ["Track"],
            None,
            "{track?: id|name, model?: 'whisper-base', models_dir?: path, language?: 'en', diarize?: bool} — \
             speech-to-text for a track (undoable); needs a build with the speech feature and the speech model on \
             disk: it is looked for in $PA_SPEECH_MODELS_DIR, then the app bundle's resources, then the application \
             data folder (`models_dir` overrides the search); nothing downloads it yet; a track longer than 4 hours, \
             or too long to hold in memory at its sample rate (2^29 samples, about 3.1 hours at 48 kHz), is refused, \
             never cut short. Called here (control channel, MCP, CLI) it runs on the calling thread and returns when \
             the transcript is stored; the app's own menu and command palette run it as a background job in the \
             Transcript window instead",
            always,
            transcribe
        ),
        cmd!(
            "track.set_transcript",
            "Set Track Transcript",
            [],
            None,
            "{track: id|name, words: [{start, end, text, speaker?: 0..255}], language?, source?} — stores a transcript, \
             replacing the track's (undoable); `start`/`end` are samples from session start; `text` must not be empty; \
             `words: []` stores an empty one",
            always,
            set_transcript
        ),
    ]
}

/// A recogniser that threads share (`Transcriber` is `Send + Sync`).
pub type SharedTranscriber = Arc<dyn soundcraft_pa_speech::Transcriber>;

/// Where a job gets its recogniser.
enum Recogniser {
    /// The speech model on disk: `models_dir` is where it is (or why it was not found), loaded when
    /// the job runs. This is what every job of the program is.
    OnDisk { model: String, models_dir: std::result::Result<PathBuf, SpeechError> },
    /// One the caller brought ([`transcribe_job_with_transcriber`]).
    Given(SharedTranscriber),
}

/// `track.transcribe`'s inputs, gathered on the calling thread: the track's audio as mono at the
/// session's sample rate, and where the recogniser comes from. [`TranscribeJob::run`] does the
/// (slow) resampling and recognition and may run on any thread; it never touches the session.
pub struct TranscribeJob {
    /// The track being transcribed.
    pub track: TrackId,
    sample_rate: SampleRate,
    /// The track downmixed to mono, at `sample_rate` (not yet at the recogniser's 16 kHz).
    audio: Vec<f32>,
    recogniser: Recogniser,
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
/// looked for where the running program would find it ([`ModelSearch::from_environment`], with the
/// brand's folder names), when the job runs.
pub fn transcribe_job(e: &Engine, p: &Value) -> Result<TranscribeJob> {
    let search = ModelSearch::from_environment(soundcraft_brand::CONFIG_DIR_NAME, soundcraft_brand::CONFIG_DIR_XDG);
    transcribe_job_limits(e, p, Source::Search(&search), &Limits::DEFAULT)
}

/// [`transcribe_job`] with the recogniser given by the caller instead of looked for on disk: the job
/// runs `transcriber` on the track's audio, and the speech model is never searched for. This is the
/// seam that tests of the app's menu, command palette and Transcript window use to prove that a
/// background job ends in a stored transcript without needing the model installed. The program's own
/// entry points call [`transcribe_job`]; they never reach this by default.
pub fn transcribe_job_with_transcriber(e: &Engine, p: &Value, transcriber: SharedTranscriber) -> Result<TranscribeJob> {
    transcribe_job_limits(e, p, Source::Given(transcriber), &Limits::DEFAULT)
}

/// [`transcribe_job`] with the places to look for the model given (tests pass their own, so no
/// process environment is touched).
#[cfg(test)]
fn transcribe_job_with(e: &Engine, p: &Value, search: &ModelSearch) -> Result<TranscribeJob> {
    transcribe_job_limits(e, p, Source::Search(search), &Limits::DEFAULT)
}

/// Where [`transcribe_job_limits`] gets the recogniser from.
enum Source<'a> {
    Search(&'a ModelSearch),
    Given(SharedTranscriber),
}

/// How much audio one transcription may gather, and how it is rendered. The fixed values are the
/// constants above; tests lower them so the edges can be reached without gigabytes of audio.
#[derive(Clone, Copy, Debug)]
struct Limits {
    /// Longest track, in seconds.
    max_secs: usize,
    /// Most samples in the mono, session-rate buffer.
    max_samples: usize,
    /// Seconds of the track rendered per call.
    chunk_secs: usize,
}

impl Limits {
    const DEFAULT: Limits = Limits { max_secs: MAX_TRANSCRIBE_SECS, max_samples: MAX_MONO_SAMPLES, chunk_secs: RENDER_CHUNK_SECS };
}

fn transcribe_job_limits(e: &Engine, p: &Value, source: Source, limits: &Limits) -> Result<TranscribeJob> {
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

    let recogniser = match source {
        Source::Given(transcriber) => Recogniser::Given(transcriber),
        Source::Search(search) => {
            let model = str_param(p, "model").unwrap_or(soundcraft_pa_speech::models::DEFAULT_MODEL).to_string();
            // An explicit folder wins (and is not second-guessed: if the model is not in it, that is
            // the answer). Otherwise search.
            let models_dir = match str_param(p, "models_dir") {
                Some(dir) => Ok(PathBuf::from(dir)),
                None => resolve_models_dir(search, &model),
            };
            Recogniser::OnDisk { model, models_dir }
        }
    };

    // Gather the track's clips as mono at the session rate, refusing a track that is too long
    // before rendering any of it. The conversion to 16 kHz is `run`'s: it is the slow part, and
    // this runs on the thread the person is waiting on.
    let sr = e.session().sample_rate;
    let end = track_end(e.session(), track);
    check_length_with(end, sr, limits)?;
    let channels = e.session().track(track).map_or(1, |t| t.channels());
    let reserve = soundcraft_mix::clip_fx_preroll_samples(e.session(), track);
    let audio = render_mono(e.session(), track, end, chunk_samples(sr, channels, limits.chunk_secs, reserve))?;

    let opts = soundcraft_pa_speech::Options {
        language: str_param(p, "language").map(str::to_string),
        diarize: p.get("diarize").and_then(Value::as_bool).unwrap_or(true),
        max_speakers: 6,
    };
    Ok(TranscribeJob { track, sample_rate: sr, audio, recogniser, opts })
}

/// Where `track`'s audio ends, in samples from the session start (0 for a track with no clips).
/// This track's own end, not the session's: a short track in a long session is not "too long".
fn track_end(s: &Session, track: TrackId) -> Samples {
    s.track(track).and_then(|t| t.playlist()).map_or(0, |p| p.end()).max(0)
}

/// Samples per render call for a track of `channels` channels: [`Limits::chunk_secs`] seconds, but
/// never so many that `soundcraft_mix::render_clips` would cut the render short (it caps one
/// render at `MAX_RENDER_SAMPLES` summed over the track's channels). `run_in` is the audio that is
/// rendered ahead of each range and thrown away ([`soundcraft_mix::clip_fx_preroll_samples`]); it
/// counts against that cap too. At least one.
fn chunk_samples(sr: SampleRate, channels: usize, chunk_secs: usize, run_in: usize) -> usize {
    let wanted = usize::try_from(sr.hz()).unwrap_or(usize::MAX).saturating_mul(chunk_secs.max(1));
    let room = (soundcraft_mix::MAX_RENDER_SAMPLES / channels.max(1)).saturating_sub(run_in);
    wanted.min(room).max(1)
}

/// `track`'s clips over `0..end` as one mono buffer at the session rate: the track is rendered
/// `chunk` samples at a time ([`chunk_samples`]) and each range is averaged over its channels
/// straight into the buffer, so neither the render cap across channels nor a full-length
/// multichannel copy is ever in play. A clip effect (EQ, compressor with lookahead) keeps state, so
/// each range is rendered after a run-in of earlier audio that settles it and is thrown away
/// (`soundcraft_mix::render_clips_settled`): a range boundary does not cost the audio around it
/// anything, and the result equals one render of the whole track. The caller has checked `end`
/// against the limits (the buffer is sized from it). Errors rather than return fewer than `end`
/// samples.
fn render_mono(s: &Session, track: TrackId, end: Samples, chunk: usize) -> Result<Vec<f32>> {
    let total = usize::try_from(end.max(0)).unwrap_or(usize::MAX);
    let chunk = chunk.max(1);
    let mut mono: Vec<f32> = Vec::with_capacity(total);
    let mut start = 0usize;
    while start < total {
        let len = chunk.min(total - start);
        let range = Range::new(i64::try_from(start).unwrap_or(i64::MAX), i64::try_from(start + len).unwrap_or(i64::MAX));
        let part = soundcraft_pa_speech::downmix(&soundcraft_mix::render_clips_settled(s, track, range));
        if part.len() != len {
            return Err(bad(TRANSCRIBE, format!("rendering stopped early ({} of {len} samples at {start}); nothing was transcribed", part.len())));
        }
        mono.extend_from_slice(&part);
        start += len;
    }
    Ok(mono)
}

/// Why a `model_unavailable` outcome happened, as a code a UI can act on: `not_found` (put the
/// model where it is looked for), `unsupported_build` (this build cannot recognise speech whatever
/// is on disk), `unknown_model` (the id is not in the catalogue) or `invalid_model` (the files are
/// there but cannot be used).
fn unavailable_reason(err: &soundcraft_pa_speech::SpeechError) -> &'static str {
    use soundcraft_pa_speech::SpeechError as E;
    match err {
        E::ModelUnavailable { .. } | E::NotInstalled(_) => "not_found",
        E::Unavailable(_) => "unsupported_build",
        E::UnknownModel(_) => "unknown_model",
        _ => "invalid_model",
    }
}

/// What to call a transcript's `source` when showing it to people: for a model of the catalogue
/// (`"whisper-base"`) its neutral display name, never the id; anything else (a source an agent
/// stored with `track.set_transcript`) as it was given.
pub fn source_label(source: &str) -> String {
    soundcraft_pa_speech::models::find(source).map_or_else(|| source.to_string(), |m| m.name.to_string())
}

/// What to do about a `model_unavailable` outcome: where the app looks for the model, and what the
/// build needs. Nothing here promises a download, because nothing downloads the model yet.
fn model_hint() -> String {
    let looked_for = "the speech model (the folder `whisper-base` with its four files) is looked for in $PA_SPEECH_MODELS_DIR, then the app \
                      bundle's resources, then the application data folder (`error` lists the exact folders and says why a folder was \
                      refused); pass `models_dir` to use another folder; nothing downloads the model yet, so put the folder there by hand";
    if soundcraft_pa_speech::available() {
        looked_for.to_string()
    } else {
        format!("this build does not include the speech feature, so it cannot recognise speech whatever is on disk; {looked_for}")
    }
}

/// Refuse a track that is longer than the limits allow before any of it is rendered: more than
/// [`MAX_TRANSCRIBE_SECS`] (the recogniser's own limit), or more samples at its rate than the mono
/// buffer may hold ([`MAX_MONO_SAMPLES`]). `end` is the track's length in samples at `sr`.
fn check_length_with(end: Samples, sr: SampleRate, limits: &Limits) -> Result<()> {
    let samples = u64::try_from(end.max(0)).unwrap_or(0);
    let rate = sr.as_f64().max(1.0);
    let secs = samples as f64 / rate;
    // How long a track may be at this rate: the time limit, or what fits in the buffer.
    let fits_secs = limits.max_samples as f64 / rate;
    let (max_secs, memory_bound) = if fits_secs < limits.max_secs as f64 { (fits_secs, true) } else { (limits.max_secs as f64, false) };
    // Exact in samples (not in seconds) at the edge: a track of exactly the limit is allowed.
    let over = secs > limits.max_secs as f64 || samples > limits.max_samples as u64;
    if over {
        let because = if memory_bound { format!(" at {} Hz, because the audio has to fit in memory", sr.hz()) } else { String::new() };
        return Err(bad(
            TRANSCRIBE,
            format!(
                "the track is {:.1} hours long; transcription is limited to {:.1} hours{because} (shorten the track first)",
                secs / 3600.0,
                max_secs / 3600.0
            ),
        ));
    }
    Ok(())
}

/// How many samples `from_hz` audio of `len` samples has at the recogniser's 16 kHz (what the
/// resampler returns), without converting anything.
fn samples_at_recogniser_rate(len: usize, from_hz: u32) -> usize {
    if from_hz == 0 || from_hz == soundcraft_pa_speech::SAMPLE_RATE {
        return len;
    }
    (len as f64 * f64::from(soundcraft_pa_speech::SAMPLE_RATE) / f64::from(from_hz)).round() as usize
}

impl TranscribeJob {
    /// Length of the audio to recognise, in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.audio.len() as f64 / self.sample_rate.as_f64().max(1.0)
    }

    /// How many samples the job carries: the track at the session's rate, mono. The conversion to
    /// the recogniser's rate has not happened yet; [`TranscribeJob::run`] does it.
    pub fn gathered_samples(&self) -> usize {
        self.audio.len()
    }

    /// Recognise the speech. `progress(fraction, status)` is called as it goes; returning `false`
    /// stops it (an error that says so). A model that cannot be found or loaded is not an error:
    /// the outcome's `status` is `model_unavailable`, its `error` says why (and lists the places
    /// searched), and it carries no transcript.
    ///
    /// The audio is converted to the recogniser's 16 kHz here, after the model has been found (a
    /// missing model is reported at once, not after minutes of conversion), so that this is the one
    /// place the slow work happens and the caller's thread never does it.
    pub fn run(self, progress: soundcraft_pa_speech::ProgressFn) -> Result<TranscribeOutcome> {
        let TranscribeJob { track, sample_rate: sr, audio, recogniser, opts } = self;
        let failed = |err: SpeechError| bad(TRANSCRIBE, format!("transcription failed: {err}"));

        // A model must be on disk (and the speech feature built in) to recognise speech.
        let transcriber = match recogniser {
            Recogniser::Given(transcriber) => transcriber,
            Recogniser::OnDisk { model, models_dir } => match models_dir.and_then(|dir| soundcraft_pa_speech::load(&dir, &model)) {
                Ok(t) => t,
                Err(err) => {
                    // `model` is the id agents pass back; `model_name`, `reason` and `why` are for
                    // showing people (no folder paths, no model id); `error` is the full text for
                    // agents and logs, with every place searched.
                    let result = json!({
                        "status": "model_unavailable", "track": track.0, "model": model, "model_name": soundcraft_pa_speech::models::label(&model),
                        "reason": unavailable_reason(&err), "why": err.summary(), "error": err.to_string(),
                        "samples_16k": samples_at_recogniser_rate(audio.len(), sr.hz()), "hint": model_hint()
                    });
                    return Ok(TranscribeOutcome { result, transcript: None });
                }
            },
        };

        if !progress(0.0, "Preparing audio") {
            return Err(failed(SpeechError::Cancelled));
        }
        // High-quality (Kaiser-windowed sinc, anti-aliased) rate conversion to the recogniser's
        // 16 kHz. The session-rate buffer is dropped as soon as it has been converted.
        let audio = soundcraft_dsp::offline::resample(&[audio], sr.hz(), soundcraft_pa_speech::SAMPLE_RATE).into_iter().next().unwrap_or_default();

        let produced = transcriber.transcribe(&audio, &opts, progress).map_err(failed)?;

        let words = session_words(&produced.words, sr);
        let count = words.len();
        let text = produced.text();
        let (language, source) = (produced.language.clone(), produced.source.clone());
        let result = json!({"status": "ok", "track": track.0, "words": count, "language": language, "source": source, "text": text});
        Ok(TranscribeOutcome { result, transcript: Some(Transcript { track, words, language, source, speaker_names: Vec::new() }) })
    }
}

/// The recogniser's words as session words (origin: session start), held to exactly the rules
/// `track.set_transcript` enforces, so a job's transcript is always accepted when a UI stores it
/// through that command: text trimmed and cut to [`MAX_WORD_BYTES`], words with no text dropped,
/// `0 <= start <= end <= MAX_POSITION`.
fn session_words(words: &[soundcraft_pa_speech::Word], sr: SampleRate) -> Vec<TranscriptWord> {
    words
        .iter()
        .filter_map(|w| {
            let text = clip_text(w.text.trim(), MAX_WORD_BYTES);
            if text.is_empty() {
                return None;
            }
            let start = sr.samples(w.start_sec).clamp(0, MAX_POSITION);
            let end = sr.samples(w.end_sec).clamp(start, MAX_POSITION);
            Some(TranscriptWord { start, end, text: text.to_string(), speaker: w.speaker })
        })
        .collect()
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
        let text = w.get("text").and_then(Value::as_str).ok_or_else(|| bad(SET_TRANSCRIPT, format!("words[{i}].text must be a string")))?.trim();
        if text.is_empty() {
            return Err(bad(SET_TRANSCRIPT, format!("words[{i}].text is empty")));
        }
        // A missing speaker is speaker 0; one that is given must be a whole number 0..=255 (it is
        // never silently turned into 0 or clamped to 255).
        let speaker = match w.get("speaker") {
            None => 0,
            Some(s) => s
                .as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| bad(SET_TRANSCRIPT, format!("words[{i}].speaker must be a whole number from 0 to 255 (got {s})")))?,
        };
        words.push(TranscriptWord {
            start: start.min(MAX_POSITION),
            end: end.min(MAX_POSITION),
            text: clip_text(text, MAX_WORD_BYTES).to_string(),
            speaker,
        });
    }
    // Stored transcripts are in time order (stable, so equal starts keep their given order).
    words.sort_by_key(|w| (w.start, w.end));
    let language: String = str_param(p, "language").unwrap_or("").chars().take(16).collect();
    let source: String = str_param(p, "source").unwrap_or("").chars().take(64).collect();
    let count = words.len();
    e.session_mut().set_transcript(Transcript { track, words, language, source, speaker_names: Vec::new() });
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

    /// The default limits.
    fn check_length(end: Samples, sr: SampleRate) -> Result<()> {
        check_length_with(end, sr, &Limits::DEFAULT)
    }

    /// Whatever the recogniser returns, the job's words must be accepted by `track.set_transcript`
    /// (the path the Transcript window stores them through): a blank or inverted word must not
    /// make a whole transcription fail to store.
    #[test]
    fn job_words_always_pass_set_transcript() {
        use soundcraft_pa_speech::Word;
        let w = |s: f64, e: f64, t: &str| Word { start_sec: s, end_sec: e, text: t.to_string(), speaker: 1 };
        let produced = vec![
            w(0.0, 0.5, "hello"),
            w(0.5, 0.6, "   "),
            w(0.6, 0.7, ""),
            w(-1.0, 0.8, " world "),
            w(1.2, 1.0, "inverted"),
            w(1.3, f64::NAN, "nan"),
            w(1.4, 1.5, &"é".repeat(MAX_WORD_BYTES)),
        ];
        let sr = SampleRate::new(48_000).unwrap();
        let words = session_words(&produced, sr);
        assert_eq!(words.iter().map(|w| w.text.as_str()).take(2).collect::<Vec<_>>(), ["hello", "world"]);
        assert_eq!(words.len(), 5, "only the two blank words are dropped");
        assert!(words.iter().all(|w| 0 <= w.start && w.start <= w.end && !w.text.is_empty() && w.text.len() <= MAX_WORD_BYTES));

        let mut e = demo_engine();
        let t = first_audio(&e);
        let stored = serde_json::to_value(&words).unwrap();
        let r = e.execute(SET_TRANSCRIPT, &json!({"track": t.0, "words": stored})).unwrap();
        assert_eq!(r["words"], 5);
    }

    #[test]
    fn set_transcript_stores_sorted_words_and_replaces() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let words = json!([
            {"start": 48_000, "end": 60_000, "text": " world ", "speaker": 1},
            {"start": 0, "end": 12_000, "text": "hello", "speaker": 255},
        ]);
        let r = e.execute(SET_TRANSCRIPT, &json!({"track": t.0, "words": words, "language": "en", "source": "test"})).unwrap();
        assert_eq!(r["words"], 2);
        let got = e.session().transcript(t).unwrap();
        assert_eq!(got.text(), "hello world");
        assert_eq!(got.words[0].speaker, u8::MAX);
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
            // a speaker that is given must be a whole number 0..=255: never 0, never clamped
            json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": "x", "speaker": 256}]}),
            json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": "x", "speaker": 300}]}),
            json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": "x", "speaker": -1}]}),
            json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": "x", "speaker": 1.5}]}),
            json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": "x", "speaker": "2"}]}),
            json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": "x", "speaker": null}]}),
            // a word with no text is not a word
            json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": ""}]}),
            json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": "  \t\n "}]}),
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

    #[test]
    fn both_transcript_commands_are_undoable() {
        for id in [SET_TRANSCRIPT, TRANSCRIBE] {
            let spec = crate::cmd::find_command(id).unwrap();
            assert!(spec.undoable, "{id} replaces document data, so it must be undoable");
        }
    }

    #[test]
    fn undo_restores_the_previous_transcript() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let set = |e: &mut Engine, text: &str| {
            e.execute(SET_TRANSCRIPT, &json!({"track": t.0, "words": [{"start": 0, "end": 10, "text": text}], "language": "en", "source": "test"}))
                .unwrap();
        };
        assert!(e.session().transcript(t).is_none());

        set(&mut e, "first");
        set(&mut e, "second");
        assert_eq!(e.session().transcript(t).unwrap().text(), "second");
        assert_eq!(e.undo_label(), Some("Set Track Transcript"));

        assert!(e.undo());
        assert_eq!(e.session().transcript(t).unwrap().text(), "first", "undo brings back the transcript that was replaced");
        assert!(e.undo());
        assert!(e.session().transcript(t).is_none(), "and before that there was none");
        assert!(e.redo() && e.redo());
        assert_eq!(e.session().transcript(t).unwrap().text(), "second");

        // Clearing is undoable too.
        e.execute(SET_TRANSCRIPT, &json!({"track": t.0, "words": []})).unwrap();
        assert!(e.undo());
        assert_eq!(e.session().transcript(t).unwrap().text(), "second");
    }

    #[test]
    fn a_refused_set_transcript_adds_no_undo_step() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let steps = e.undo_history().len();
        assert!(e.execute(SET_TRANSCRIPT, &json!({"track": t.0, "words": [{"start": 0, "end": 5, "text": "x", "speaker": 999}]})).is_err());
        assert_eq!(e.undo_history().len(), steps);
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
        assert!(error.starts_with("Speech model, base (multilingual) not found (searched:"), "{error}");
        assert!(!error.to_lowercase().contains("whisper"), "{error}");
        assert!(
            error.contains(&*scratch.0.join("env").to_string_lossy()) && error.contains(&*scratch.0.join("support").to_string_lossy()),
            "{error}"
        );
        assert!(out.result["samples_16k"].as_u64().unwrap() > 0);
        assert!(out.transcript.is_none());
        // Agents get the id (`model`) and the full text (`error`); the fields meant for people name
        // the model by its display name and leave out the model id and the folders searched.
        assert_eq!(out.result["model"], "whisper-base");
        assert_eq!(out.result["model_name"], "Speech model, base (multilingual)");
        assert_eq!(out.result["reason"], "not_found");
        assert_eq!(out.result["why"], "Speech model, base (multilingual) was not found");
        for shown in ["model_name", "why"] {
            let text = out.result[shown].as_str().unwrap();
            assert!(!text.to_lowercase().contains("whisper") && !text.contains(&*scratch.0.to_string_lossy()), "{shown}: {text}");
        }
        // The hint says where the model is looked for and promises no download.
        let hint = out.result["hint"].as_str().unwrap();
        assert!(hint.contains("PA_SPEECH_MODELS_DIR") && hint.contains("app bundle") && hint.contains("application data folder"), "{hint}");
        assert!(hint.contains("models_dir") && hint.contains("nothing downloads the model yet"), "{hint}");
        assert!(!hint.contains("Transcription Settings"), "{hint}");
        assert_eq!(hint.contains("does not include the speech feature"), !soundcraft_pa_speech::available(), "{hint}");
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
        assert_eq!(out.result["error"], "Speech model, base (multilingual) is not installed");
        assert_eq!((out.result["reason"].as_str(), out.result["model"].as_str()), (Some("not_found"), Some("whisper-base")));
    }

    #[test]
    fn model_folders_are_named_from_the_brand() {
        let home = |name: &str| (name == "HOME").then(|| std::ffi::OsString::from("/home/gil"));
        let (name, xdg) = (soundcraft_brand::CONFIG_DIR_NAME, soundcraft_brand::CONFIG_DIR_XDG);
        let mac = ModelSearch::from_parts(soundcraft_pa_speech::Platform::Mac, name, xdg, home, None);
        let expected = std::path::Path::new("/home/gil/Library/Application Support").join(soundcraft_brand::CONFIG_DIR_NAME).join("models");
        assert_eq!(mac.app_support, Some(expected));
        // On the other Unixes the folder is the brand's XDG name, the same one the settings use.
        let other = ModelSearch::from_parts(soundcraft_pa_speech::Platform::Other, name, xdg, home, None);
        let expected = std::path::Path::new("/home/gil/.local/share").join(soundcraft_brand::CONFIG_DIR_XDG).join("models");
        assert_eq!(other.app_support, Some(expected));
    }

    #[test]
    fn a_track_longer_than_the_cap_is_refused_before_anything_is_rendered() {
        // The 4-hour limit, at a rate where it is the tighter one (32 kHz: 4 h is under 2^29 samples).
        let sr = SampleRate::new(32_000).unwrap();
        let cap = MAX_TRANSCRIBE_SECS as i64 * 32_000;
        assert!(check_length(0, sr).is_ok());
        assert!(check_length(cap, sr).is_ok(), "exactly the cap is allowed");
        let msg = check_length(cap + 32_000, sr).unwrap_err().to_string();
        assert!(msg.contains("4.0 hours long") && msg.contains("limited to 4.0 hours") && !msg.contains("memory"), "{msg}");
        assert!(check_length(i64::MAX, sr).is_err(), "no overflow");
        assert!(check_length(-5, sr).is_ok());

        // And through the command: move a clip to just past the cap; the job (and so the
        // command) refuses without rendering the 4+ hours of audio that would otherwise be built.
        let mut e = demo_engine();
        let t = first_audio(&e);
        let clip = e.session().tracks.iter().find(|x| x.id == t).and_then(|x| x.clips().first().map(|c| c.id)).unwrap();
        let hz = i64::from(e.session().sample_rate.hz());
        e.session_mut().find_clip_mut(clip).unwrap().start = (MAX_TRANSCRIBE_SECS as i64 + 60) * hz;
        let search = ModelSearch::default();
        let refused = transcribe_job_with(&e, &json!({"track": t.0}), &search).err().expect("too long");
        assert!(refused.to_string().contains("hours long"), "{refused}");
        let steps = e.undo_history().len();
        assert!(e.execute(TRANSCRIBE, &json!({"track": t.0})).is_err());
        assert_eq!(e.undo_history().len(), steps);
    }

    /// The refusal reads as one sentence with one parenthesis, whichever limit it is.
    #[test]
    fn the_refusal_text_has_one_set_of_parentheses() {
        let memory = check_length(MAX_MONO_SAMPLES as i64 + 1, SampleRate::HZ_48000).unwrap_err().to_string();
        assert!(memory.contains("limited to 3.1 hours at 48000 Hz, because the audio has to fit in memory (shorten the track first)"), "{memory}");
        let hours = check_length(MAX_TRANSCRIBE_SECS as i64 * 32_000 + 32_000, SampleRate::new(32_000).unwrap()).unwrap_err().to_string();
        assert!(hours.ends_with("limited to 4.0 hours (shorten the track first)"), "{hours}");
        for msg in [&memory, &hours] {
            assert_eq!((msg.matches('(').count(), msg.matches(')').count()), (1, 1), "{msg}");
            assert!(!msg.contains(") ("), "{msg}");
        }
    }

    /// The mono buffer has a memory cap of its own, which is the tighter limit at common rates:
    /// 2^29 samples is 3.1 hours at 48 kHz. A longer track is refused, saying how long one may be.
    #[test]
    fn a_track_too_long_to_hold_in_memory_is_refused_with_the_limit_for_its_rate() {
        assert_eq!(MAX_MONO_SAMPLES, 1 << 29);
        let sr = SampleRate::HZ_48000;
        let cap = MAX_MONO_SAMPLES as i64;
        assert!(check_length(cap, sr).is_ok(), "exactly the cap is allowed");
        let msg = check_length(cap + 1, sr).unwrap_err().to_string();
        assert!(msg.contains("3.1 hours long") && msg.contains("limited to 3.1 hours at 48000 Hz") && msg.contains("fit in memory"), "{msg}");
        // The 4-hour session of the old limit is refused at 48 kHz and fine at 24 kHz.
        let four_hours = MAX_TRANSCRIBE_SECS as i64 * 48_000;
        assert!(check_length(four_hours, sr).is_err());
        assert!(check_length(MAX_TRANSCRIBE_SECS as i64 * 24_000, SampleRate::new(24_000).unwrap()).is_ok());
    }

    /// The reported case: a stereo clip at 25:00 on a 192 kHz session. One 2^29-sample render across
    /// both channels stops at 23.3 minutes, and the command answered `ok` for the part it had.
    #[test]
    fn a_clip_beyond_the_old_render_cap_is_covered_by_ranges_not_cut() {
        let sr = SampleRate::new(192_000).unwrap();
        let end = (25 * 60 + 10) * 192_000;
        assert!(end > soundcraft_mix::MAX_RENDER_SAMPLES as i64 / 2, "past what a one-shot stereo render holds");
        assert!(check_length(end, sr).is_ok(), "it fits the mono buffer, so it is transcribed in full");
        let chunk = chunk_samples(sr, 2, RENDER_CHUNK_SECS, 0);
        assert_eq!(chunk, 60 * 192_000);
        assert!(chunk * 2 <= soundcraft_mix::MAX_RENDER_SAMPLES, "every range is under the render cap");
        // Ranges of this size cover the track exactly, with no gap at the old cap.
        let mut covered = 0usize;
        while (covered as i64) < end {
            covered += chunk.min(end as usize - covered);
        }
        assert_eq!(covered as i64, end);
        // 50 minutes at 192 kHz would not fit the mono buffer: refused, never `ok` and short.
        let refused = check_length(50 * 60 * 192_000, sr).unwrap_err().to_string();
        assert!(refused.contains("limited to") && refused.contains("192000 Hz") && refused.contains("fit in memory"), "{refused}");
    }

    #[test]
    fn a_range_is_never_longer_than_the_render_cap_allows() {
        let hz192 = SampleRate::new(192_000).unwrap();
        for channels in [0usize, 1, 2, 6, 16, 64, 1000, usize::MAX] {
            for sr in [SampleRate::HZ_48000, hz192] {
                for secs in [0usize, 1, 60, 3600, usize::MAX] {
                    let n = chunk_samples(sr, channels, secs, 0);
                    assert!(n >= 1, "{channels} {secs}");
                    if channels <= soundcraft_mix::MAX_RENDER_SAMPLES {
                        assert!(n.saturating_mul(channels.max(1)) <= soundcraft_mix::MAX_RENDER_SAMPLES, "{channels} channels x {n}");
                    }
                }
            }
        }
        assert_eq!(chunk_samples(SampleRate::HZ_48000, 2, 60, 0), 2_880_000);
        // 64 channels at 192 kHz: a minute would be 737 million samples, so the range shrinks.
        assert_eq!(chunk_samples(hz192, 64, 60, 0), soundcraft_mix::MAX_RENDER_SAMPLES / 64);
        // The run-in rendered ahead of a range counts against the same cap: the range gives way to it.
        let run_in = 30 * 192_000;
        let n = chunk_samples(hz192, 64, 60, run_in);
        assert_eq!(n, soundcraft_mix::MAX_RENDER_SAMPLES / 64 - run_in);
        assert!((n + run_in) * 64 <= soundcraft_mix::MAX_RENDER_SAMPLES, "range and run-in fit the cap together");
        assert_eq!(chunk_samples(SampleRate::HZ_48000, 2, 60, 3 * 48_000), 2_880_000, "a minute still fits next to 3 s");
        assert_eq!(chunk_samples(hz192, 2, 60, usize::MAX), 1, "never zero");
    }

    /// What the job rendered before it rendered in ranges: one call over the whole track.
    fn one_shot_mono(e: &Engine, t: TrackId) -> Vec<f32> {
        let end = track_end(e.session(), t);
        soundcraft_pa_speech::downmix(&soundcraft_mix::render_clips(e.session(), t, Range::new(0, end)))
    }

    #[test]
    fn rendering_in_ranges_equals_one_render() {
        let e = demo_engine();
        let t = first_audio(&e);
        let end = track_end(e.session(), t);
        let reference = one_shot_mono(&e, t);
        assert_eq!(reference.len() as i64, end);
        assert!(reference.iter().any(|x| x.abs() > 0.01), "the demo track has audio");
        // Aligned and unaligned range sizes, one range, and ranges of a single sample's worth.
        for chunk in [32usize, 4_096, 10_001, 48_000, 100_003, reference.len(), reference.len() + 5, usize::MAX] {
            let ranged = render_mono(e.session(), t, end, chunk).unwrap();
            assert_eq!(ranged.len(), reference.len(), "chunk {chunk}");
            let worst = ranged.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst <= 1e-4, "chunk {chunk}: the largest difference from one render is {worst}");
        }
        // An empty track is an empty buffer, not an error.
        assert!(render_mono(e.session(), t, 0, 4_096).unwrap().is_empty());
    }

    #[test]
    fn a_render_that_comes_back_short_is_an_error_not_an_ok() {
        let e = demo_engine();
        // A track that does not exist renders nothing; asking for 1000 samples must not give 0 back.
        let err = render_mono(e.session(), TrackId(987_654), 1_000, 400).unwrap_err().to_string();
        assert!(err.contains("rendering stopped early"), "{err}");
    }

    #[test]
    fn a_track_is_gathered_in_full_or_refused_never_cut_short() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let hz = i64::from(e.session().sample_rate.hz());
        // Shift the track's clips 3 s later (so the front is silence) and take the new end as the
        // limit, with ranges of 1 s: the same arithmetic as a clip at 25:00 with 60 s ranges.
        let ids: Vec<_> = e.session().track(t).unwrap().clips().iter().map(|c| c.id).collect();
        for id in ids {
            e.session_mut().find_clip_mut(id).unwrap().start += 3 * hz;
        }
        let end = track_end(e.session(), t);
        assert!(end > 4 * hz, "the demo track is longer than a second");
        let limits = Limits { max_secs: MAX_TRANSCRIBE_SECS, max_samples: end as usize, chunk_secs: 1 };
        let search = ModelSearch::default();
        let job = transcribe_job_limits(&e, &json!({"track": t.0}), Source::Search(&search), &limits).unwrap();
        // Exactly at the limit is allowed, and all of it is there: to the last sample, resampled.
        assert!((job.duration_secs() - end as f64 / hz as f64).abs() < 0.01, "{} s of {} s", job.duration_secs(), end as f64 / hz as f64);
        let mono = render_mono(e.session(), t, end, chunk_samples(e.session().sample_rate, 2, 1, 0)).unwrap();
        assert_eq!(mono.len() as i64, end);
        assert!(mono[..(3 * hz) as usize].iter().all(|x| *x == 0.0), "the 3 s moved in front of the clips are silence");
        assert!(mono[(end - 2 * hz) as usize..].iter().any(|x| x.abs() > 1e-3), "the last two ranges hold the end of the track");
        let reference = one_shot_mono(&e, t);
        let worst = mono.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(mono.len() == reference.len() && worst <= 1e-4, "ranges and one render agree ({worst})");

        // One sample over the limit: refused, with the limit in the message, and nothing rendered.
        let tight = Limits { max_samples: end as usize - 1, ..limits };
        let refused = transcribe_job_limits(&e, &json!({"track": t.0}), Source::Search(&search), &tight).err().expect("one sample too long");
        assert!(refused.to_string().contains("limited to"), "{refused}");
    }

    // ---- the job: the render is the caller's, the conversion and the recognition are the worker's ----

    /// A recogniser that keeps the audio it was given and answers with `words`.
    struct Recorder {
        heard: std::sync::Mutex<Vec<f32>>,
        words: Vec<soundcraft_pa_speech::Word>,
    }

    impl Recorder {
        fn new(words: &[(&str, f64, f64)]) -> Arc<Recorder> {
            let words = words.iter().map(|(t, a, b)| soundcraft_pa_speech::Word::new(*t, *a, *b)).collect();
            Arc::new(Recorder { heard: std::sync::Mutex::new(Vec::new()), words })
        }
        fn heard(&self) -> Vec<f32> {
            self.heard.lock().unwrap().clone()
        }
    }

    impl soundcraft_pa_speech::Transcriber for Recorder {
        fn id(&self) -> String {
            "recorder".into()
        }
        fn transcribe(
            &self,
            audio: &[f32],
            _: &soundcraft_pa_speech::Options,
            progress: soundcraft_pa_speech::ProgressFn,
        ) -> std::result::Result<soundcraft_pa_speech::Transcript, SpeechError> {
            *self.heard.lock().unwrap() = audio.to_vec();
            if !progress(0.5, "Recording") {
                return Err(SpeechError::Cancelled);
            }
            Ok(soundcraft_pa_speech::Transcript { words: self.words.clone(), language: "en".into(), source: self.id() })
        }
    }

    /// The job holds the track at the session's rate; `run` converts it. What the recogniser hears
    /// is what the old pipeline (render, downmix, resample, all on the calling thread) produced.
    #[test]
    fn the_job_converts_on_run_and_the_recogniser_hears_what_it_always_did() {
        let e = demo_engine();
        let t = first_audio(&e);
        let sr = e.session().sample_rate;
        let end = track_end(e.session(), t);
        let recorder = Recorder::new(&[("hello", 0.5, 1.0), ("world", 1.25, 2.0)]);
        let job = transcribe_job_with_transcriber(&e, &json!({"track": t.0}), recorder.clone()).unwrap();

        assert_eq!(job.gathered_samples() as i64, end, "the job carries the track as rendered: session rate, mono, not yet resampled");
        assert!((job.duration_secs() - end as f64 / sr.as_f64()).abs() < 1e-9);
        assert!(recorder.heard().is_empty(), "nothing has been recognised yet");

        let out = job.run(&mut |_, _| true).unwrap();
        let reference = soundcraft_dsp::offline::resample(&[one_shot_mono(&e, t)], sr.hz(), soundcraft_pa_speech::SAMPLE_RATE).remove(0);
        let heard = recorder.heard();
        assert_eq!(heard.len(), reference.len(), "the same number of 16 kHz samples");
        assert_eq!(heard.len(), samples_at_recogniser_rate(end as usize, sr.hz()));
        let worst = heard.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst <= 1e-6, "the recogniser hears the old pipeline's audio ({worst})");
        assert!(heard.iter().any(|x| x.abs() > 0.01), "and it is not silence");

        // The result is what `track.transcribe` always reported, in samples from the session start.
        assert_eq!(out.result["status"], "ok");
        assert_eq!(
            (out.result["words"].as_u64(), out.result["text"].as_str(), out.result["source"].as_str()),
            (Some(2), Some("hello world"), Some("recorder"))
        );
        let stored = out.transcript.expect("a transcript to store");
        assert_eq!(stored.track, t);
        assert_eq!((stored.words[0].start, stored.words[0].end), (sr.samples(0.5), sr.samples(1.0)));
        assert_eq!((stored.words[1].start, stored.words[1].end), (sr.samples(1.25), sr.samples(2.0)));
    }

    /// The report: `transcribe_job` ran the sample-rate conversion (about 40 s for an hour of audio)
    /// on the thread the menu or palette was waiting on. It must return having done only the render.
    /// Structure first (the job holds 48 kHz audio, three times what 16 kHz would be), then, since
    /// the two are measured in one process on one machine, that gathering costs less than the
    /// conversion alone.
    #[test]
    fn a_long_track_is_gathered_without_being_converted() {
        let seconds = 240usize;
        let n = seconds * 48_000;
        let tone: Vec<f32> = (0..n).map(|i| 0.3 * (i as f32 * 0.031).sin()).collect();
        let mut s = Session::default();
        let t = s.add_track(TrackKind::Audio, soundcraft_model::ChannelFormat::Mono, None);
        s.pool.insert(
            soundcraft_model::SourceId(77),
            Arc::new(soundcraft_model::SourceAudio::new(soundcraft_audio_io::AudioBuffer { sample_rate: 48_000, channels: vec![tone.clone()] })),
        );
        let id = s.new_clip_id();
        s.track_mut(t).unwrap().playlist_mut().unwrap().clips.push(soundcraft_model::Clip::audio(
            id,
            "long",
            soundcraft_model::SourceId(77),
            0,
            0,
            n as i64,
        ));
        assert_eq!(s.sample_rate.hz(), 48_000);
        let e = Engine::new(s);

        let recorder = Recorder::new(&[]);
        let started = std::time::Instant::now();
        let job = transcribe_job_with_transcriber(&e, &json!({"track": t.0}), recorder.clone()).unwrap();
        let gathered_in = started.elapsed();
        assert_eq!(job.gathered_samples(), n, "{seconds} s at 48 kHz, not the {} samples it would be at 16 kHz", n / 3);
        assert!((job.duration_secs() - seconds as f64).abs() < 1e-9);

        let started = std::time::Instant::now();
        let converted = soundcraft_dsp::offline::resample(&[tone], 48_000, soundcraft_pa_speech::SAMPLE_RATE);
        let conversion = started.elapsed();
        assert_eq!(converted[0].len(), n / 3);
        assert!(
            gathered_in < conversion,
            "gathering took {gathered_in:?}, the conversion alone {conversion:?}: the job is doing the conversion up front"
        );
        // `run` is where it happens.
        job.run(&mut |_, _| true).unwrap();
        assert_eq!(recorder.heard().len(), n / 3);
    }

    /// A job built with `transcribe_job_with_transcriber` never looks for a model, whatever
    /// `models_dir` says; one built with `transcribe_job` always does.
    #[test]
    fn a_given_transcriber_skips_the_search_and_the_ordinary_job_does_not() {
        let e = demo_engine();
        let t = first_audio(&e);
        let nowhere = std::env::temp_dir().join(format!("pa-tools-given-{}", std::process::id()));
        let params = json!({"track": t.0, "models_dir": nowhere.to_string_lossy()});
        let given = transcribe_job_with_transcriber(&e, &params, Recorder::new(&[("ask", 0.0, 0.5)])).unwrap().run(&mut |_, _| true).unwrap();
        assert_eq!(given.result["status"], "ok");
        assert_eq!(given.transcript.as_ref().map(|t| t.words.len()), Some(1));
        let ordinary = transcribe_job(&e, &params).unwrap().run(&mut |_, _| true).unwrap();
        assert_eq!(ordinary.result["status"], "model_unavailable", "the ordinary job reads `models_dir` and finds nothing there");
        assert!(ordinary.transcript.is_none());
    }

    #[test]
    fn stopping_while_the_audio_is_prepared_ends_the_job_with_an_error() {
        let e = demo_engine();
        let t = first_audio(&e);
        let recorder = Recorder::new(&[("x", 0.0, 0.1)]);
        let job = transcribe_job_with_transcriber(&e, &json!({"track": t.0}), recorder.clone()).unwrap();
        let err = job.run(&mut |_, _| false).expect_err("stopped");
        assert!(err.to_string().contains("stopped"), "{err}");
        assert!(recorder.heard().is_empty(), "the conversion and the recogniser never ran");
    }

    // ---- clip effects: a range boundary leaves no mark on the audio --------------------------

    /// The report: `render_clips` started the clip effects afresh at each 60 s range, so a track with
    /// clip EQ, a compressor and lookahead lost up to 10 ms of audio and stepped in gain at every
    /// seam. Ranges rendered with their run-in equal one render; the old way does not.
    #[test]
    fn ranges_equal_one_render_on_a_track_with_clip_effects() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let ids: Vec<_> = e.session().track(t).unwrap().clips().iter().map(|c| c.id).collect();
        for id in ids {
            for (k, v) in [
                ("eq.hpf_on", 1.0),
                ("eq.hpf_freq", 90.0),
                ("eq.mid_gain", 6.0),
                ("comp.threshold", -30.0),
                ("comp.ratio", 6.0),
                ("comp.attack", 4.0),
                ("comp.release", 300.0),
                ("comp.lookahead", 8.0),
            ] {
                e.session_mut().edit.values.insert(format!("clip_fx.{}.{k}", id.0), v);
            }
        }
        let end = track_end(e.session(), t);
        let reference = one_shot_mono(&e, t);
        assert!(reference.iter().any(|x| x.abs() > 0.01));

        // Ranges of about a second, so the track crosses a dozen seams, none on a block boundary.
        let chunk = 48_000 + 777;
        assert!(end as usize / chunk >= 8, "the demo track crosses at least eight seams");
        let settled = render_mono(e.session(), t, end, chunk).unwrap();
        assert_eq!(settled.len(), reference.len());
        let worst = settled.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst <= 1e-5, "ranges with a run-in differ from one render by {worst}");

        // The control: each range rendered on its own, as before, is audibly different.
        let mut naive = Vec::new();
        let mut at = 0usize;
        while (at as i64) < end {
            let len = chunk.min(end as usize - at);
            let range = Range::new(at as i64, (at + len) as i64);
            naive.extend(soundcraft_pa_speech::downmix(&soundcraft_mix::render_clips(e.session(), t, range)));
            at += len;
        }
        let bad = naive.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(bad > 1e-3 && bad > 100.0 * worst, "a render per range differs by {bad}: the effects must matter here or this proves nothing");
    }

    #[test]
    fn a_transcripts_source_is_shown_by_its_display_name() {
        assert_eq!(source_label("whisper-base"), "Speech model, base (multilingual)");
        assert_eq!(source_label("whisper-tiny"), "Speech model, tiny (multilingual)");
        // Anything else is what an agent stored, as it was given.
        assert_eq!(source_label("manual"), "manual");
        assert_eq!(source_label(""), "");
    }

    #[test]
    fn transcribe_command_matches_the_job_path() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let dir = std::env::temp_dir().join(format!("pa-tools-no-models-cmd-{}", std::process::id()));
        let steps = e.undo_history().len();
        let r = e.execute(TRANSCRIBE, &json!({"track": t.0, "models_dir": dir.to_string_lossy()})).unwrap();
        assert_eq!(r["status"], "model_unavailable");
        assert!(e.session().transcript(t).is_none());
        // Nothing was stored, so there is nothing to undo.
        assert_eq!(e.undo_history().len(), steps);
    }
}
