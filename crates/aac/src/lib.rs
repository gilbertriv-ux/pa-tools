//! Clean-room AAC-LC encoder and decoder (ISO/IEC 14496-3, MPEG-4 Audio, Low Complexity profile), with
//! writers for `.m4a` and ADTS (`.aac`) files.
//!
//! **Everything that makes AAC is behind the Cargo feature `aac-encode`, which is off by default and off
//! in the shipped app** (patent posture, `docs/audio-codecs.md`). Without it this crate has only
//! [`Error`], the limits and [`ENCODER_BUILT`] (`false`), and the writers answer [`Error::NotBuilt`], so
//! a caller needs no `cfg` of its own. `cargo test -p soundcraft-aac --features aac-encode` is the real
//! test run.
//!
//! ## Files
//!
//! - [`encode_m4a`] / [`M4aWriter`]: MPEG-4 audio file. The encoder delay (1024 samples) is an edit list
//!   whose length is the input length, so a player reproduces the input exactly.
//! - [`encode_adts`] / [`AdtsWriter`]: raw ADTS stream. ADTS cannot signal the delay; a player starts the
//!   audio with the 1024 priming samples.
//!
//! Input is interleaved `f32`, mono or stereo, at one of the 13 AAC sample rates (7.35 to 96 kHz);
//! NaN and infinity become silence and finite values are clamped to full scale.
//!
//! ## Codec
//!
//! - [`Encoder`]: planar f32 → raw access units (+ [`Encoder::audio_specific_config`] for MP4 `esds`,
//!   optional ADTS framing). Psychoacoustic model, window switching, M/S, TNS, bit reservoir, CBR/VBR,
//!   1–8 channels.
//! - [`Decoder`]: `AudioSpecificConfig` + raw access units → planar f32. Supports M/S, intensity
//!   stereo, PNS, TNS, pulse data, both window shapes and every window sequence.
//!
//! HE-AAC (SBR/PS), Main/SSR/LTP profiles, coupling channels and 960-sample frames are not supported;
//! for explicitly signalled HE-AAC only the AAC-LC core is decoded (at the core rate).
//!
//! Layer L0: depends only on `soundcraft-bitstream` and `soundcraft-isobmff` (both L0, and only with
//! `aac-encode`) plus `thiserror` and optional `rayon` (feature `rayon`, used only by the encoder and not
//! implying `aac-encode`); no `unsafe`; builds for `wasm32-unknown-unknown`.
//!
//! ```
//! # #[cfg(feature = "aac-encode")]
//! # fn main() -> Result<(), soundcraft_aac::Error> {
//! use soundcraft_aac::{Decoder, Encoder, EncoderConfig};
//!
//! let mut enc = Encoder::new(EncoderConfig::cbr(48_000, 2, 128_000))?;
//! let left: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
//! let right = left.clone();
//! let mut aus = enc.encode(&[&left, &right]); // 0..n access units
//! aus.extend(enc.flush());
//! let asc = enc.audio_specific_config(); // for the MP4 `esds`
//! assert_eq!(enc.priming_samples(), 1024); // MP4 edit list media_time
//!
//! let mut dec = Decoder::new(&asc)?;
//! for au in &aus {
//!     let pcm = dec.decode(au)?; // planar, 1024 samples per channel
//!     assert_eq!(pcm.len(), 2);
//! }
//!
//! let m4a = soundcraft_aac::encode_m4a(&vec![0.1; 2 * 48_000], 2, 48_000, 128_000)?;
//! assert_eq!(&m4a[4..8], b"ftyp");
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "aac-encode"))]
//! # fn main() {}
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
// Index loops mirror the specifications' pseudo-code and keep the DSP loops readable.
#![allow(clippy::needless_range_loop)]

#[cfg(feature = "aac-encode")]
mod config;
#[cfg(feature = "aac-encode")]
mod decoder;
#[cfg(feature = "aac-encode")]
mod encoder;
mod export;
#[cfg(feature = "aac-encode")]
mod huffman;
#[cfg(feature = "aac-encode")]
mod huffman_tables;
#[cfg(feature = "aac-encode")]
mod ics;
#[cfg(feature = "aac-encode")]
mod mdct;
#[cfg(feature = "aac-encode")]
mod tables;
#[cfg(feature = "aac-encode")]
mod tns;

#[cfg(feature = "aac-encode")]
pub use config::{AdtsHeader, AudioSpecificConfig, ElementType, ProgramConfig, config_layout, split_adts};
#[cfg(feature = "aac-encode")]
pub use decoder::Decoder;
#[cfg(feature = "aac-encode")]
pub use encoder::{BitrateMode, Encoder, EncoderConfig, EncoderStats};
pub use export::{
    AacOptions, AdtsWriter, EXPORT_SAMPLE_RATES, M4aWriter, MAX_BITRATE_PER_CHANNEL, MAX_SECONDS, MIN_BITRATE_PER_CHANNEL, Stats, bitrate_range,
    encode_adts, encode_adts_with, encode_m4a, encode_m4a_with,
};
#[cfg(feature = "aac-encode")]
pub use mdct::WindowShape;
#[cfg(feature = "aac-encode")]
pub use tables::{SAMPLE_RATES, sample_rate_index};

/// Whether this build contains the AAC encoder (the Cargo feature `aac-encode`).
pub const ENCODER_BUILT: bool = cfg!(feature = "aac-encode");

/// Samples per AAC-LC frame (per channel).
pub const FRAME_LEN: usize = 1024;

/// Errors from configuration parsing, encoding and decoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("unsupported feature: {0}")]
    Unsupported(&'static str),
    #[error("invalid bitstream: {0}")]
    Bitstream(&'static str),
    #[error("unexpected end of access unit")]
    Eof,
    /// The build has no AAC encoder (the Cargo feature `aac-encode` is off).
    #[error("AAC export is not included in this build")]
    NotBuilt,
    #[error("AAC export takes 1 or 2 channels, not {0}")]
    InvalidChannels(u16),
    #[error(
        "sample rate {0} Hz is not one of the AAC rates (7350, 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000)"
    )]
    InvalidSampleRate(u32),
    #[error("bitrate {bitrate} bit/s is outside {min} to {max} bit/s for this format")]
    InvalidBitrate { bitrate: u32, min: u32, max: u32 },
    #[error("{len} samples are not a whole number of {channels}-channel frames")]
    Misaligned { len: usize, channels: usize },
    #[error("there is no audio to encode")]
    Empty,
    #[error("the audio is longer than 24 hours")]
    TooLong,
    #[error("invalid file tag: {0}")]
    InvalidMetadata(&'static str),
    #[error("write failed: {0}")]
    Io(String),
    #[error("container error: {0}")]
    Container(String),
}

#[cfg(feature = "aac-encode")]
impl From<soundcraft_bitstream::BitError> for Error {
    fn from(_: soundcraft_bitstream::BitError) -> Self {
        Error::Eof
    }
}

pub type Result<T> = std::result::Result<T, Error>;
