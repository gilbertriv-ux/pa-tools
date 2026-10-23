//! OMF Interchange 2.0 export (and import of the same structures) for audio post.
//!
//! Written from the public *OMF Interchange Specification Version 2.0* (Avid Technology, 1995/97)
//! and Apple's *Bento Specification* revision 1.0d5 for the container (`bento`).
//!
//! The file holds a composition mob (`CMOB`) with a timecode slot and one sound slot per audio
//! track (one per channel when broken out to mono): sequences (`SEQU`) of fillers (`FILL`), source
//! clips (`SCLP`) and transitions (`TRAN` with a `SimpleMonoAudioDissolve` effect); clip volume is a
//! `MonoAudioGain` effect (`EFFE`) whose slot −1 is the clip and slot 1 the amplitude
//! (`CVAL` constant or `VVAL` keyframes). Every media item has a master mob (`MMOB`), a file source
//! mob (`SMOB` with a `WAVD` / `AIFD` descriptor: embedded essence in a `WAVE` media data object,
//! or a locator to a separate file) and a tape source mob carrying its timecode. Times are in
//! samples (sound slots use the sample rate as edit rate) so edits are sample-exact.
//!
//! OMF has no picture here and no nested compositions: a session maps to the sound slots only.

pub(crate) mod bento;
mod read;
mod write;

#[cfg(test)]
mod tests;

use crate::comp::Document;
use crate::{Report, Result};

/// Serialise the sound of a composition document as an OMF 2.0 file.
pub(crate) fn write_document(doc: &Document, report: &mut Report) -> Result<Vec<u8>> {
    write::write(doc, report)
}

/// Read the compositions and media of an OMF 2.0 file.
pub(crate) fn read_document(bytes: &[u8], report: &mut Report) -> Result<Document> {
    read::read(bytes, report)
}

/// Whether `bytes` end with a Bento container label (OMF files do).
pub fn sniff(bytes: &[u8]) -> bool {
    bento::sniff(bytes)
}
