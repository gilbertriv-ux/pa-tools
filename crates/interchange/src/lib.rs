//! SoundCraft interchange (milestone M1: container + identification only).
//!
//! - [`Cfb`]: a clean-room reader for Microsoft Compound File Binary containers (the public
//!   MS-CFB layout). It treats its input as hostile: every offset and length is checked, every
//!   allocation is bounded by the input length, chains are cycle-checked, and nothing panics.
//! - [`Cfb::identify`] / [`AafIdentity`]: decides whether a container looks like an AAF file.
//! - [`Cfb::aaf_header`] / [`AafHeader`]: the few header facts that can be read without a full
//!   AAF object model.
//! - [`Composition`]: the neutral tracks/clips model an AAF import produces. [`Cfb::composition`]
//!   is the entry point; the object-model decoding that fills it needs a real `.aaf` sample to pin
//!   the class/property GUIDs and is not implemented yet.
//!
//! Not supported: writing, AAF object/composition decoding (the mapping into [`Composition`]),
//! import into sessions.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod aaf;
mod cfb;
mod composition;

pub use aaf::{AAF_ROOT_CLSID, AafHeader, AafIdentity, Confidence, PropertyIndexEntry};
pub use cfb::{Cfb, Clsid, Entry, EntryKind, MAX_ENTRIES, MAX_FILE_BYTES};
pub use composition::{CompClip, CompTrack, Composition, TrackKind};

/// Errors from reading a container.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InterchangeError {
    #[error("not a Compound File (bad or missing signature)")]
    NotCfb,
    #[error("truncated: {0}")]
    Truncated(&'static str),
    #[error("malformed container: {0}")]
    Malformed(&'static str),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("input too large ({0} bytes)")]
    TooLarge(u64),
    #[error("no such stream: {0}")]
    NoSuchStream(String),
    #[error("i/o error: {0}")]
    Io(String),
}
