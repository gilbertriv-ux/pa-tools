//! AAF (Advanced Authoring Format) import and export, following the AAF Edit Protocol.
//!
//! Written from the public AMWA specifications: *AAF Object Specification v1.1*, *AAF Low-Level
//! Container Specification v1.0.1* (structured storage mapping, see [`store`]) and *AAF Edit
//! Protocol v1.0*; the container is Microsoft's Compound File Binary format ([`soundcraft_cfb`]).
//!
//! The exported file holds one top-level **CompositionMob** (Edit Protocol usage code
//! "top level") with
//!
//! - a timecode slot (sequence start timecode, rounded rate, drop frame),
//! - one timeline slot per video track (picture) and per audio track (sound; one per channel when
//!   broken out to mono), each a **Sequence** of **Filler**s, **SourceClip**s and **Transition**s
//!   (Cross Dissolve → `VideoDissolve_2`, Dip to Black → `VideoFadeToBlack`, audio crossfades →
//!   `MonoAudioDissolve`; the fade shape is kept in a tagged value),
//! - clip volume as a `MonoAudioGain` **OperationGroup** around the source clip with an
//!   `Amplitude` **ConstantValue** or **VaryingValue** (keyframes as control points),
//! - sequence markers as **CommentMarker**s in an event slot;
//!
//! a **CompositionMob** without a usage code for every nested sequence, built the same way (a
//! clip of the nested sequence is a **SourceClip** that points at that mob, at its first slot of
//! the clip's kind, with its start in that slot's edit units; Premiere Pro writes nested sequences
//! this way);
//!
//! and for every media item a **MasterMob**, a file **SourceMob** per essence (a CDCI or PCM
//! descriptor with a network locator to the file, or `EssenceData` holding embedded PCM) and a
//! tape **SourceMob** carrying the media's start timecode. Clip and media markers become comment
//! markers on the master mob.
//!
//! Import reads the same structures (and the common variations: sequences inside sequences,
//! selectors, operation groups around clips, legacy data definitions, essence groups, and Premiere
//! Pro's one video slot holding a nested scope with a segment per video track) into the
//! composition model of [`crate::comp`], which [`crate::session`] maps to tracks, clips and markers
//! of a session (embedded audio is returned as WAV files for the caller to write).

mod ids;
mod read;
pub(crate) mod store;
mod write;

use crate::comp::Document;
use crate::{Report, Result};

/// Serialise a composition document as an AAF file.
pub(crate) fn write_document(doc: &Document, small_sectors: bool, report: &mut Report) -> Result<Vec<u8>> {
    let version = if small_sectors { soundcraft_cfb::Version::V3 } else { soundcraft_cfb::Version::V4 };
    write::write(doc, version, report)
}

/// Read the compositions and media of an AAF file.
pub(crate) fn read_document(bytes: &[u8], report: &mut Report) -> Result<Document> {
    read::read(bytes, report)
}

/// Whether `bytes` look like an AAF file (a compound file whose root has the AAF root class or a
/// `Header-2` storage).
pub fn sniff(bytes: &[u8]) -> bool {
    if !soundcraft_cfb::sniff(bytes) {
        return false;
    }
    match soundcraft_cfb::CompoundFile::open(bytes) {
        Ok(cf) => cf.root().clsid == ids::ROOT || cf.find("Header-2").is_ok(),
        Err(_) => false,
    }
}
