//! Speech-to-text for Pa Tools (Text ▸ Transcript).
//!
//! Standalone, layer-0 crate: no `soundcraft-*` dependency. Ported from FilmCraft's
//! `filmcraft-speech` (MIT OR Apache-2.0); the donor's `filmcraft-time` (`Tick`) and
//! `filmcraft-project` (`Transcript`, `Word`) types are **inlined** here as plain `f64` seconds so
//! the crate carries no workspace dependency. The engine converts `*_sec` to `soundcraft-time`
//! ticks at the wiring boundary.
//!
//! - [`Transcriber`]: the trait every recogniser implements — mono 16 kHz samples in, a
//!   [`Transcript`] with word-level time bounds out.
//! - [`FixedTranscriber`]: returns a supplied transcript (tests, demos, agents that bring their
//!   own transcript); words that end past the clip's end are dropped. No model.
//! - [`models`]: the catalogue of downloadable Whisper models (source pinned to a revision,
//!   SHA-256, size, licence) and where they live. Weights are **never** bundled or committed.
//! - [`discover`]: finds installed model weights at run time ([`ModelSearch`], [`resolve_models_dir`]):
//!   `$PA_SPEECH_MODELS_DIR`, then the app bundle's resources, then the user's application data folder.
//! - [`load`]: builds the Whisper transcriber (feature `whisper`).
//!
//! Feature `whisper` adds candle Whisper inference (`whisper/`), the log-mel front end (`mel`),
//! energy-based bound tightening (`vad`) and speaker labelling (`diarize`), working internally in
//! exact ticks ([`tick`]/[`ptranscript`]) and converting to the public `f64`-seconds API at the
//! [`Transcriber`] boundary. Feature `download` adds the verified HTTPS model downloader.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod discover;
pub mod models;

#[cfg(feature = "whisper")]
mod diarize;
#[cfg(feature = "whisper")]
mod mel;
#[cfg(feature = "whisper")]
mod ptranscript;
#[cfg(feature = "whisper")]
mod tick;
#[cfg(feature = "whisper")]
mod vad;
#[cfg(feature = "whisper")]
pub mod whisper;

pub use discover::{ModelSearch, Platform, resolve_models_dir};

use serde::{Deserialize, Serialize};

/// The sample rate every transcriber takes.
pub const SAMPLE_RATE: u32 = 16_000;

/// The longest audio, in seconds, that a recogniser accepts (4 hours). Longer is refused with
/// [`SpeechError::TooLong`] before anything is allocated for it: the recogniser holds several
/// spectrogram-sized buffers for the whole clip (about 100 MB per hour for the mel front end
/// alone, more for speaker labelling), and the decoder walks it in 30 s windows.
pub const MAX_AUDIO_SECS: usize = 4 * 3600;

/// [`MAX_AUDIO_SECS`] in samples at [`SAMPLE_RATE`].
pub const MAX_AUDIO_SAMPLES: usize = MAX_AUDIO_SECS * SAMPLE_RATE as usize;

/// `Ok` when `samples` (at [`SAMPLE_RATE`]) is within [`MAX_AUDIO_SAMPLES`], else
/// [`SpeechError::TooLong`] saying how long it is and what the limit is.
pub fn check_length(samples: usize) -> Result<(), SpeechError> {
    if samples <= MAX_AUDIO_SAMPLES {
        return Ok(());
    }
    let hours = samples as f64 / f64::from(SAMPLE_RATE) / 3600.0;
    Err(SpeechError::TooLong(format!("{hours:.1} hours; the limit is {} hours", MAX_AUDIO_SECS / 3600)))
}

/// A transcribed word with its time bounds in seconds (relative to the first sample).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// Start time in seconds.
    pub start_sec: f64,
    /// End time in seconds.
    pub end_sec: f64,
    /// The word text.
    pub text: String,
    /// Speaker label (0 when diarization is off).
    pub speaker: u8,
}

impl Word {
    /// A word spanning `[start_sec, end_sec]`, speaker 0.
    pub fn new(text: impl Into<String>, start_sec: f64, end_sec: f64) -> Self {
        Self { start_sec, end_sec, text: text.into(), speaker: 0 }
    }
}

/// A transcript: words in time order, the detected language, and the model id that produced it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// Words in time order.
    pub words: Vec<Word>,
    /// ISO 639-1 language code (`""` if unknown).
    pub language: String,
    /// Stable id of the model that produced this (`"whisper-base"`, `"fixed"`).
    pub source: String,
}

impl Transcript {
    /// The plain text, words joined by single spaces.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SpeechError {
    #[error("stopped")]
    Cancelled,
    #[error("the speech model `{0}` is not downloaded")]
    NotInstalled(String),
    /// No searched folder has the model; the text names the model and every place looked in.
    #[error("{0}")]
    ModelUnavailable(String),
    #[error("unknown speech model `{0}`")]
    UnknownModel(String),
    #[error("speech-to-text is not available in this build ({0})")]
    Unavailable(String),
    #[error("model: {0}")]
    Model(String),
    /// The audio is longer than [`MAX_AUDIO_SECS`]; the text says by how much.
    #[error("the audio is too long to transcribe ({0})")]
    TooLong(String),
    #[error("download: {0}")]
    Download(String),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for SpeechError {
    fn from(e: std::io::Error) -> Self {
        SpeechError::Io(e.to_string())
    }
}

/// Transcription options.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// ISO 639-1 language code; `None` = detect from the first 30 s.
    pub language: Option<String>,
    /// Label speakers; otherwise every word gets speaker 0.
    pub diarize: bool,
    /// Upper bound for the number of speakers found.
    pub max_speakers: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { language: None, diarize: true, max_speakers: 6 }
    }
}

/// Progress callback: `(fraction 0..1, status)`; return `false` to cancel.
pub type ProgressFn<'a> = &'a mut dyn FnMut(f32, &str) -> bool;

/// A speech recogniser.
pub trait Transcriber: Send + Sync {
    /// Stable id of the model (`"whisper-base"`), stored in the transcript's `source`.
    fn id(&self) -> String;
    /// Transcribe mono 16 kHz `audio`. Word times are relative to the first sample.
    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError>;
}

/// A transcriber that returns a fixed transcript (tests, demos and agents that bring their own
/// transcript); words that end past the audio's length are dropped.
pub struct FixedTranscriber {
    pub transcript: Transcript,
    pub id: String,
}

impl Transcriber for FixedTranscriber {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn transcribe(&self, audio: &[f32], _: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        if !progress(0.5, "Transcribing") {
            return Err(SpeechError::Cancelled);
        }
        let end_sec = audio.len() as f64 / SAMPLE_RATE as f64;
        let mut t = self.transcript.clone();
        // A word that ends past the clip (or has no finite bounds) is not in the audio: dropped.
        t.words.retain(|w| w.start_sec.is_finite() && w.end_sec.is_finite() && w.end_sec <= end_sec);
        t.source = self.id.clone();
        progress(1.0, "Done");
        Ok(t)
    }
}

/// Load the transcriber for catalogue model `id` from `models_dir` (feature `whisper`).
///
/// Validates the catalogue and that the model is installed, then (feature `whisper`) returns a real
/// candle-backed Whisper transcriber; without the feature it reports `Unavailable`.
pub fn load(models_dir: &std::path::Path, id: &str) -> Result<std::sync::Arc<dyn Transcriber>, SpeechError> {
    let m = models::find(id).ok_or_else(|| SpeechError::UnknownModel(id.into()))?;
    if !models::installed(models_dir, m) {
        return Err(SpeechError::NotInstalled(id.into()));
    }
    #[cfg(feature = "whisper")]
    {
        Ok(std::sync::Arc::new(whisper::Whisper::load(&models::model_dir(models_dir, m), m.id)?))
    }
    #[cfg(not(feature = "whisper"))]
    {
        let _ = m;
        Err(SpeechError::Unavailable("built without the `whisper` feature".into()))
    }
}

/// Whether this build can run speech models.
pub const fn available() -> bool {
    cfg!(feature = "whisper")
}

/// Mix a multi-channel buffer down to mono.
pub fn downmix(channels: &[Vec<f32>]) -> Vec<f32> {
    let n = channels.iter().map(Vec::len).min().unwrap_or(0);
    if channels.len() == 1 {
        return channels[0][..n].to_vec();
    }
    let k = 1.0 / channels.len().max(1) as f32;
    (0..n).map(|i| channels.iter().map(|c| c[i]).sum::<f32>() * k).collect()
}

/// Lowercase and strip non-alphanumeric characters (for [`word_error_rate`]).
fn normalize_word(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Word error rate between a reference and a hypothesis (words compared case-insensitively without
/// punctuation, hyphenated words split): (substitutions + deletions + insertions) / reference words.
pub fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let norm = |s: &str| -> Vec<String> { s.split(|c: char| c.is_whitespace() || c == '-').map(normalize_word).filter(|w| !w.is_empty()).collect() };
    let (r, h) = (norm(reference), norm(hypothesis));
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for i in 1..=r.len() {
        let mut cur = vec![i; h.len() + 1];
        for j in 1..=h.len() {
            let sub = prev[j - 1] + usize::from(r[i - 1] != h[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    prev[h.len()] as f64 / r.len() as f64
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn wer_counts_edits() {
        assert_eq!(word_error_rate("the cat sat", "The cat, sat."), 0.0);
        assert!((word_error_rate("the cat sat on the mat", "the cat sat on mat") - 1.0 / 6.0).abs() < 1e-9);
        assert!((word_error_rate("a b", "a x b y") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn audio_longer_than_the_cap_is_refused_before_any_work() {
        assert_eq!(MAX_AUDIO_SAMPLES, 4 * 3600 * 16_000);
        assert_eq!(check_length(0), Ok(()));
        assert_eq!(check_length(MAX_AUDIO_SAMPLES), Ok(()));
        let err = check_length(MAX_AUDIO_SAMPLES + 1).unwrap_err();
        assert!(matches!(err, SpeechError::TooLong(_)));
        assert!(err.to_string().contains("4.0 hours; the limit is 4 hours"), "{err}");
        assert!(check_length(usize::MAX).is_err(), "no overflow on absurd lengths");
    }

    #[test]
    fn downmix_averages() {
        assert_eq!(downmix(&[vec![1.0, 0.0], vec![0.0, 0.0, 9.0]]), vec![0.5, 0.0]);
    }

    #[test]
    fn transcript_text_joins_words() {
        let t = Transcript { words: vec![Word::new("hello", 0.0, 0.4), Word::new("world", 0.5, 0.9)], language: "en".into(), source: "fixed".into() };
        assert_eq!(t.text(), "hello world");
    }
}
