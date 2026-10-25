//! The neutral composition an AAF import maps to (the M2 target).
//!
//! This is the result an AAF parse produces: tracks and the clips on them, independent of the AAF
//! object model. The session importer converts this to session tracks/clips. The AAF object-model
//! decoding that *fills* this — Mobs, MobSlots and Segments resolved through the metadata
//! dictionary's class/property GUIDs — is written against a real `.aaf` sample and is not
//! implemented yet (see [`crate::Cfb::composition`]); the container and identification layers it
//! builds on are complete.
//!
//! Times are in the composition's own edit units ([`Composition::edit_rate`] per second); the
//! importer converts them to the session sample rate when it lands the clips.

use crate::{Cfb, InterchangeError};

/// What a track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Video,
    /// A slot we can read but do not map onto a session track (timecode, data, …).
    Other,
}

/// One clip on a track: a reference to a source and where it sits on the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompClip {
    /// How to find the media: the source mob's name or file reference, resolved later to a path.
    pub source: String,
    /// Clip start on the track timeline, in edit units from the composition start (>= 0).
    pub start: i64,
    /// Clip length in edit units (> 0).
    pub length: i64,
    /// Offset into the source where the clip begins, in edit units (>= 0).
    pub source_offset: i64,
}

/// One track (an AAF MobSlot) and its clips, in timeline order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompTrack {
    pub name: Option<String>,
    pub kind: TrackKind,
    pub clips: Vec<CompClip>,
}

/// A neutral composition: the tracks and clips an AAF file describes. The importer turns this into
/// session tracks and clips.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composition {
    /// Edit units per second (0 when unknown).
    pub edit_rate: u32,
    pub tracks: Vec<CompTrack>,
}

impl Composition {
    /// Total clips across every track.
    pub fn clip_count(&self) -> usize {
        self.tracks.iter().map(|t| t.clips.len()).sum()
    }

    /// The audio tracks only (what the first import cares about).
    pub fn audio_tracks(&self) -> impl Iterator<Item = &CompTrack> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Audio)
    }
}

impl Cfb {
    /// Parses the AAF composition in this container into a neutral [`Composition`].
    ///
    /// Not implemented yet: mapping the AAF object model (Mobs/MobSlots/Segments decoded through the
    /// metadata dictionary's class/property GUIDs) needs a real `.aaf` sample to pin those GUIDs and
    /// their byte layouts, so this returns [`InterchangeError::Unsupported`] rather than guessing a
    /// layout that could silently mis-import clips. The container and identification layers this
    /// builds on are complete and tested.
    pub fn composition(&self) -> Result<Composition, InterchangeError> {
        if !self.identify().is_aaf() {
            return Err(InterchangeError::Unsupported("not an AAF container"));
        }
        Err(InterchangeError::Unsupported("AAF composition mapping is not implemented yet (needs a real .aaf sample to pin the object-model GUIDs)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(source: &str, start: i64, length: i64) -> CompClip {
        CompClip { source: source.into(), start, length, source_offset: 0 }
    }

    fn comp() -> Composition {
        Composition {
            edit_rate: 48_000,
            tracks: vec![
                CompTrack { name: Some("A1".into()), kind: TrackKind::Audio, clips: vec![clip("vo.wav", 0, 100), clip("music.wav", 100, 50)] },
                CompTrack { name: Some("V1".into()), kind: TrackKind::Video, clips: vec![clip("reel.mov", 0, 150)] },
                CompTrack { name: None, kind: TrackKind::Other, clips: vec![] },
            ],
        }
    }

    #[test]
    fn clip_count_sums_every_track() {
        assert_eq!(comp().clip_count(), 3);
        assert_eq!(Composition::default().clip_count(), 0);
    }

    #[test]
    fn audio_tracks_filters_to_audio_only() {
        let c = comp();
        let audio: Vec<_> = c.audio_tracks().collect();
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].name.as_deref(), Some("A1"));
        assert_eq!(audio[0].clips.len(), 2);
    }

    #[test]
    fn a_default_composition_is_empty() {
        let c = Composition::default();
        assert_eq!(c.edit_rate, 0);
        assert!(c.tracks.is_empty());
        assert_eq!(c.audio_tracks().count(), 0);
    }
}
