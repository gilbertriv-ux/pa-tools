//! A format-neutral mob-style composition model shared by AAF and OMF.
//!
//! Both formats describe a timeline as a composition of slots (tracks) whose segments are
//! sequences of fillers, source clips and transitions, where a transition *overlaps* the end of
//! the segment before it and the start of the segment after it (the sequence's length is the sum of
//! its segment lengths minus its transition lengths). Source clips reference media through master
//! mobs and file source mobs. [`crate::session`] maps a Pa Tools session to and from this shape;
//! the format serialisers in [`crate::aaf`] and [`crate::omf`] convert it to and from edit units.
//! All times are [`Tick`]s.

use crate::ticks::{FrameRate, TICKS_PER_SECOND, Tick};
use crate::{Error, Result};

/// Picture or sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CKind {
    Picture,
    Sound,
}

/// A whole document: compositions and the media they reference.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Document {
    /// The top-level compositions.
    pub compositions: Vec<Composition>,
    pub sources: Vec<Source>,
    /// Compositions that are only used inside others (nested sequences); a [`Source`] refers to
    /// one by its index here. They may nest further.
    pub nested: Vec<Composition>,
}

impl Document {
    /// Check that everything the document refers to is in it: every clip names a source, and every
    /// nested-composition source names a composition. The writers rely on this (they also look
    /// everything up with `get`, so a document that skips the check fails instead of panicking).
    pub fn check(&self) -> Result<()> {
        let bad = |what: &str| Err(Error::Other(format!("inconsistent document: {what}")));
        for s in &self.sources {
            if s.nested.is_some_and(|n| n >= self.nested.len()) {
                return bad("a source names a nested composition that does not exist");
            }
        }
        for comp in self.compositions.iter().chain(&self.nested) {
            for item in comp.tracks.iter().flat_map(|t| &t.items) {
                if let CItem::Clip(c) = item
                    && c.source >= self.sources.len()
                {
                    return bad("a clip names a source that does not exist");
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Composition {
    pub name: String,
    pub rate: FrameRate,
    pub sample_rate: u32,
    pub width: u32,
    pub height: u32,
    pub start_tc: i64,
    pub drop: bool,
    pub tracks: Vec<CTrack>,
    pub markers: Vec<CMarker>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CTrack {
    pub kind: CKind,
    pub name: String,
    /// Physical track number (1-based, per kind).
    pub number: u32,
    /// Sound: channels of the track (1 mono, 2 stereo, 6 5.1).
    pub channels: u32,
    pub items: Vec<CItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CItem {
    Filler(Tick),
    Clip(CClip),
    Transition(CTransition),
}

impl CItem {
    pub fn len(&self) -> Tick {
        match self {
            CItem::Filler(l) => *l,
            CItem::Clip(c) => c.len,
            CItem::Transition(t) => t.len,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CClip {
    pub len: Tick,
    /// Index into [`Document::sources`].
    pub source: usize,
    /// Source (file) time of the segment's first frame.
    pub start: Tick,
    pub gain: Option<Gain>,
    pub name: String,
}

/// Clip gain (linear amplitude).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Gain {
    Constant(f64),
    /// Points at offsets from the segment start; `linear` false = hold (step) interpolation.
    Varying {
        linear: bool,
        points: Vec<(Tick, f64)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CTransition {
    pub len: Tick,
    /// Where the cut lies, from the transition start.
    pub cut: Tick,
    /// The id of the fade shape (`equal_power`, `linear`, ...); the legacy ids of other writers are
    /// understood on import (see `session::shape_of`).
    pub effect: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CMarker {
    pub start: Tick,
    pub duration: Tick,
    pub name: String,
    pub comment: String,
    pub color: Option<String>,
}

/// One essence: a file (or embedded data) carrying the picture, or some channels of sound, of a
/// media item.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    /// Groups the sources of one media item (one master mob).
    pub key: String,
    pub name: String,
    pub kind: CKind,
    pub path: Option<String>,
    /// Sound: the (0-based) channel of the file this source exposes (broken out to mono), or
    /// `None` for all of them.
    pub channel: Option<u32>,
    /// Sound: channels this source exposes (1 when `channel` is set, else `file_channels`).
    pub channels: u32,
    /// Sound: channels of the file / embedded data.
    pub file_channels: u32,
    pub width: u32,
    pub height: u32,
    /// Picture: the frame rate of the media.
    pub frame_rate: FrameRate,
    pub sample_rate: u32,
    pub bits: u16,
    pub length: Tick,
    /// Media time of the essence's time 0 (trimmed / consolidated essence).
    pub offset: Tick,
    /// Start timecode in frames at `tc_rate`.
    pub start_tc: Option<i64>,
    pub tc_rate: FrameRate,
    pub embedded: Option<Vec<u8>>,
    pub markers: Vec<CMarker>,
    /// Not media but a nested composition: its index in [`Document::nested`]. `start` of a clip
    /// that uses it is a time in that composition.
    pub nested: Option<usize>,
}

/// The largest time read from a document (about 3.3 days).
pub(crate) const MAX_TIME: Tick = Tick(1 << 56);

/// Ticks of `n` edit units at rate `num/den` (rounded to the nearest tick).
pub(crate) fn units_to_ticks(n: i64, num: i64, den: i64) -> Tick {
    if num <= 0 || den <= 0 {
        return Tick::ZERO;
    }
    // documents are untrusted: a product too big for i128 is far outside ±MAX_TIME anyway
    let Some(t) = i128::from(n).checked_mul(i128::from(den)).and_then(|x| x.checked_mul(i128::from(TICKS_PER_SECOND))) else {
        return if n < 0 { Tick(-MAX_TIME.0) } else { MAX_TIME };
    };
    let num = i128::from(num);
    let q = t.div_euclid(num);
    let r = t.rem_euclid(num);
    // keep every time far from overflow (±3 days)
    let ticks = q.saturating_add(i128::from(r.saturating_mul(2) >= num)).clamp(i128::from(-MAX_TIME.0), i128::from(MAX_TIME.0));
    Tick(i64::try_from(ticks).unwrap_or(0))
}

/// Edit units at rate `num/den` nearest to `t`; the flag is false when `t` is not on a unit.
pub(crate) fn ticks_to_units(t: Tick, num: i64, den: i64) -> (i64, bool) {
    if num <= 0 || den <= 0 {
        return (0, false);
    }
    // i64 × i64 fits i128, and so does i64 × the tick rate
    let x = i128::from(t.0) * i128::from(num);
    let d = i128::from(den) * i128::from(TICKS_PER_SECOND);
    let q = x.div_euclid(d);
    let r = x.rem_euclid(d);
    let n = q.saturating_add(i128::from(r.saturating_mul(2) >= d));
    (i64::try_from(n.clamp(i128::from(i64::MIN), i128::from(i64::MAX))).unwrap_or(0), r == 0)
}
