//! SoundCraft video: open a movie (MP4 / QuickTime MOV) from bytes, report its picture format, and
//! decode the frame at a time to RGBA for the Video window and the Video track's thumbnails.
//!
//! - [`Movie::open`] (bytes) / [`Movie::open_source`] (any random-access [`ByteSource`]).
//! - [`Movie::info`]: size, duration, frame rate, codec, whether it can be decoded.
//! - [`Movie::frame_at`]: the frame shown at a time, decoded forward from the previous keyframe,
//!   with a small LRU of decoded frames (sequential playback keeps decoding forward).
//! - [`Movie::thumbnail_at`] / [`Movie::thumbnails`]: low-resolution timeline pictures (keyframes).
//! - Unsupported codecs open fine (so the timeline can show the clip) but frame requests return
//!   [`VideoError::Unsupported`]. Malformed files return errors; nothing here panics.
//!
//! Picture codecs: H.264 / AVC (Baseline, Main, High; progressive 8-bit 4:2:0; CAVLC and CABAC),
//! ProRes (422 Proxy/LT/Standard/HQ, 4444/XQ; shown as "PA ProRez") and Motion JPEG
//! (`jpeg`/`mjpa`). VC-3 (DNxHD / DNxHR), HEVC and AV1 tracks are recognised (`dnxhd`, `hevc`,
//! `av1`) but not decoded yet. VC-3 is always built; the HEVC and AV1 decoders are the Cargo
//! features `hevc-decode` and `av1-decode`, off by default, and a build without one reports its
//! tracks as "unsupported in this build".
//!
//! ## Adding a codec
//!
//! [`Movie`] knows no codec's internals. A codec with inter-picture prediction implements the
//! crate-private `GopDecoder` trait (`gop.rs`: feed one sample, collect reordered pictures as
//! [`convert::Yuv`], 8 or 9 to 16 bits, any subsampling, BT.601 / 709 / 2020); an intra-only codec
//! returns packed RGBA for one sample. The glue (what the sample entry says, which variants decode,
//! how a decoder is built) lives in one `adapt_<codec>.rs` file per codec, next to the decoder's own
//! `hevc/`, `av1/` or `dnx/` module.
//!
//! ## Origin
//!
//! The container, H.264 and ProRes code is a copy of the clean-room crates of our sibling app
//! FilmCraft (`storytold/filmcraft`, same organisation, MIT OR Apache-2.0): `filmcraft-bitstream`
//! → [`bitstream`], `filmcraft-isobmff` → [`isobmff`], `filmcraft-h264` → [`h264`] (without its
//! test-only synthetic stream module), `filmcraft-prores` → [`prores`]. The crafting apps share no
//! code as dependencies; each keeps its own copy. Module paths were changed and some parsers
//! hardened (checked arithmetic, bounded allocations); [`bitstream`] matches FilmCraft commit
//! `f47a1f6`. Fixes found here should be offered back upstream. [`Movie`], the pixel conversion
//! and the frame caches are SoundCraft's own.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// Codec code indexes fixed-size blocks; index loops read more clearly than iterator chains there.
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

mod adapt_av1;
mod adapt_dnx;
mod adapt_h264;
mod adapt_hevc;
#[cfg(feature = "av1-decode")]
pub mod av1;
pub mod bitstream;
pub mod convert;
mod gop;
pub mod h264;
#[cfg(feature = "hevc-decode")]
pub mod hevc;
pub mod isobmff;
mod movie;
pub mod prores;
pub mod synth;
pub mod vc3;

pub use isobmff::ByteSource;
pub use movie::{Frame, Movie, MovieInfo, SharedSource, fit_size};

/// Errors from opening or decoding a movie.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum VideoError {
    #[error("not a readable movie: {0}")]
    Container(String),
    #[error("the movie has no video track")]
    NoVideoTrack,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("could not decode the picture: {0}")]
    Decode(String),
    #[error("no picture at that time")]
    OutOfRange,
}

pub type Result<T> = std::result::Result<T, VideoError>;

/// Open a movie and return its description (convenience for importers).
pub fn probe(bytes: &[u8]) -> Result<MovieInfo> {
    Movie::open(bytes.to_vec()).map(|m| m.info().clone())
}

#[cfg(test)]
mod seam_tests;
#[cfg(test)]
mod tests;
