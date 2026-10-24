//! Per-track speech transcripts (Window ▸ Transcript).
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
    /// Names the user gave the speakers; the index is the speaker id. An empty or missing entry
    /// means the speaker is unnamed ([`Transcript::speaker_label`] then says "Speaker N"). Sessions
    /// saved before speakers could be named have none.
    #[serde(default)]
    pub speaker_names: Vec<String>,
}

impl Transcript {
    /// Longest speaker name kept, in characters.
    pub const MAX_SPEAKER_NAME_CHARS: usize = 64;

    /// The plain text, words joined by single spaces.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// What to call speaker `id`: the name the user gave, else "Speaker N" (ids count from 0).
    pub fn speaker_label(&self, id: u8) -> String {
        match self.speaker_name(id) {
            "" => format!("Speaker {id}"),
            name => name.to_string(),
        }
    }

    /// `name` as it is stored: control characters dropped, trimmed, cut to
    /// [`Transcript::MAX_SPEAKER_NAME_CHARS`] characters. Empty means "no name".
    pub fn clean_speaker_name(name: &str) -> String {
        let plain: String = name.chars().filter(|c| !c.is_control()).collect();
        plain.trim().chars().take(Self::MAX_SPEAKER_NAME_CHARS).collect()
    }

    /// The stored name of speaker `id` (`""` when unnamed).
    pub fn speaker_name(&self, id: u8) -> &str {
        self.speaker_names.get(usize::from(id)).map_or("", |n| n.trim())
    }

    /// Names speaker `id` (see [`Transcript::clean_speaker_name`]); an empty name resets it to
    /// "Speaker N". Trailing unnamed speakers are dropped, so the list never grows past the last
    /// name that is actually set.
    pub fn set_speaker_name(&mut self, id: u8, name: &str) {
        let name = Self::clean_speaker_name(name);
        let at = usize::from(id);
        if name.is_empty() {
            if let Some(slot) = self.speaker_names.get_mut(at) {
                slot.clear();
            }
        } else {
            if self.speaker_names.len() <= at {
                self.speaker_names.resize(at + 1, String::new());
            }
            if let Some(slot) = self.speaker_names.get_mut(at) {
                *slot = name;
            }
        }
        while self.speaker_names.last().is_some_and(String::is_empty) {
            self.speaker_names.pop();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn sample() -> Transcript {
        Transcript {
            track: TrackId(1),
            words: vec![TranscriptWord { start: 0, end: 10, text: "hi".into(), speaker: 0 }],
            language: "en".into(),
            source: "test".into(),
            speaker_names: Vec::new(),
        }
    }

    #[test]
    fn speakers_are_unnamed_until_the_user_names_them() {
        let mut t = sample();
        assert_eq!(t.speaker_label(0), "Speaker 0");
        assert_eq!(t.speaker_label(255), "Speaker 255");
        t.set_speaker_name(2, "  Host ");
        assert_eq!(t.speaker_label(2), "Host");
        assert_eq!(t.speaker_label(1), "Speaker 1", "a gap before a named speaker stays unnamed");
        assert_eq!(t.speaker_names, vec!["", "", "Host"]);
    }

    #[test]
    fn an_empty_name_resets_and_the_list_shrinks_back() {
        let mut t = sample();
        t.set_speaker_name(0, "Ann");
        t.set_speaker_name(3, "Bob");
        t.set_speaker_name(3, "   ");
        assert_eq!(t.speaker_names, vec!["Ann"]);
        t.set_speaker_name(0, "");
        assert!(t.speaker_names.is_empty());
        // Resetting a speaker that was never named changes nothing (and cannot index out of range).
        t.set_speaker_name(200, "");
        assert!(t.speaker_names.is_empty());
    }

    #[test]
    fn a_long_name_is_cut_at_a_character_boundary() {
        let mut t = sample();
        t.set_speaker_name(0, &"é".repeat(Transcript::MAX_SPEAKER_NAME_CHARS * 2));
        assert_eq!(t.speaker_names[0].chars().count(), Transcript::MAX_SPEAKER_NAME_CHARS);
    }

    #[test]
    fn a_transcript_saved_before_speakers_had_names_still_loads() {
        let old = r#"{"track":7,"words":[{"start":0,"end":5,"text":"hi"}],"language":"en","source":"whisper-base"}"#;
        let t: Transcript = serde_json::from_str(old).unwrap();
        assert!(t.speaker_names.is_empty());
        assert_eq!(t.speaker_label(0), "Speaker 0");
        let round: Transcript = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(round, t);
    }
}
