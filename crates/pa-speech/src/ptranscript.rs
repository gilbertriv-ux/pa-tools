//! Internal tick-based transcript, inlined from the donor's `filmcraft-project` transcript types
//! (only what the Whisper port uses). Converted to the public `f64`-seconds `crate::Transcript` at
//! the `Transcriber` boundary in `whisper`.

use crate::tick::Tick;

/// One transcribed word (internal, tick bounds).
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    /// The word as written, with attached punctuation. No surrounding spaces.
    pub text: String,
    /// Media time of the word's first sample.
    pub start: Tick,
    /// Media time just after the word (exclusive).
    pub end: Tick,
    /// Index into [`Transcript::speakers`].
    pub speaker: Option<u32>,
    /// Recogniser confidence 0..1.
    pub confidence: f32,
}

impl Word {
    pub fn new(text: impl Into<String>, start: Tick, end: Tick) -> Self {
        Self { text: text.into(), start, end, speaker: None, confidence: 1.0 }
    }
}

/// A speaker label.
#[derive(Clone, Debug, PartialEq)]
pub struct Speaker {
    pub name: String,
}

/// The transcript of one clip (internal).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    pub language: String,
    pub source: String,
    pub speakers: Vec<Speaker>,
    /// Words in time order, non-overlapping.
    pub words: Vec<Word>,
}

impl Transcript {
    /// Sort words by time and make the transcript well formed: empty words dropped, `end >= start`,
    /// no overlaps, speaker indices in range (missing labels added as "Speaker N").
    pub fn normalize(&mut self) {
        self.words.retain(|w| !w.text.trim().is_empty());
        for w in &mut self.words {
            w.text = w.text.trim().to_string();
            if w.end < w.start {
                w.end = w.start;
            }
            w.confidence = w.confidence.clamp(0.0, 1.0);
        }
        self.words.sort_by_key(|w| (w.start, w.end));
        for i in 1..self.words.len() {
            let s = self.words[i].start;
            if self.words[i - 1].end > s {
                self.words[i - 1].end = s.max(self.words[i - 1].start);
            }
        }
        let max = self.words.iter().filter_map(|w| w.speaker).max();
        if let Some(m) = max {
            while self.speakers.len() <= m as usize {
                let n = self.speakers.len() + 1;
                self.speakers.push(Speaker { name: format!("Speaker {n}") });
            }
        }
    }

    /// Validate the invariants [`Transcript::normalize`] establishes. Test helper.
    #[cfg(test)]
    pub fn check(&self) -> Result<(), String> {
        for (i, w) in self.words.iter().enumerate() {
            if w.end < w.start {
                return Err(format!("word {i} ({:?}) ends before it starts", w.text));
            }
            if i > 0 && self.words[i - 1].end > w.start {
                return Err(format!("word {i} ({:?}) overlaps the word before it", w.text));
            }
            if let Some(s) = w.speaker
                && s as usize >= self.speakers.len()
            {
                return Err(format!("word {i} has speaker {s}, but there are {} speakers", self.speakers.len()));
            }
        }
        Ok(())
    }
}
