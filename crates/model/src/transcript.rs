//! Per-track speech transcripts (Text ▸ Transcript).
//!
//! A [`Transcript`] is produced by the speech-to-text engine (`soundcraft-pa-speech`) and stored on
//! the session.
//!
//! **Time origin (pinned):** word bounds are sample counts measured **from session start (sample
//! 0)**, at the session's sample rate — the same origin as markers and clip positions, not relative
//! to any render range. The `track.transcribe` command renders the track from sample 0, so the
//! recogniser's seconds (relative to the first sample it received) map directly to samples-from-
//! session-start; the command does that conversion when it stores the result.

use serde::{Deserialize, Serialize};
use soundcraft_time::Samples;

use crate::TrackId;

/// One transcribed word with its sample-accurate bounds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptWord {
    /// Start, in samples from session start.
    pub start: Samples,
    /// End, in samples from session start.
    pub end: Samples,
    /// The word text.
    pub text: String,
    /// Speaker label (0 when diarization is off).
    #[serde(default)]
    pub speaker: u8,
}

/// A track's transcript: words in time order, the detected language, and the model that produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// The track this transcript describes.
    pub track: TrackId,
    /// Words in time order.
    pub words: Vec<TranscriptWord>,
    /// ISO 639-1 language code (`""` if unknown).
    #[serde(default)]
    pub language: String,
    /// Stable id of the model that produced this (`"whisper-base"`).
    #[serde(default)]
    pub source: String,
}

impl Transcript {
    /// The plain text, words joined by single spaces.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}
