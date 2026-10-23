//! AAC files from interleaved f32 audio: [`encode_m4a`] / [`M4aWriter`] for `.m4a` and
//! [`encode_adts`] / [`AdtsWriter`] for raw ADTS `.aac`.
//!
//! This module is the same in every build; the functions and writers do their work only when the
//! Cargo feature `aac-encode` is on. Without it they return [`Error::NotBuilt`] and nothing else of the
//! codec is compiled in.
//!
//! Input is validated before anything is encoded: one or two channels, one of the 13 AAC sample rates
//! ([`EXPORT_SAMPLE_RATES`]), a bitrate inside [`bitrate_range`] and at most [`MAX_SECONDS`] of audio.
//! NaN and infinite samples become silence and finite ones are clamped to full scale, with a count of
//! each in [`Stats`].
//!
//! ## Timing
//!
//! The encoder delays the signal by 1024 samples and pads the end to a whole frame. In an `.m4a` the
//! `elst` edit list removes both: it starts at media time 1024 and lasts exactly the input length, in a
//! movie timescale equal to the sample rate, so a player reproduces the input sample for sample. ADTS
//! cannot say any of this; a player starts the stream with the 1024 priming samples and plays the
//! padding.

#[cfg(feature = "aac-encode")]
mod real;
#[cfg(not(feature = "aac-encode"))]
mod stub;

#[cfg(feature = "aac-encode")]
pub use real::{AdtsWriter, M4aWriter, encode_adts, encode_adts_with, encode_m4a, encode_m4a_with};
#[cfg(not(feature = "aac-encode"))]
pub use stub::{AdtsWriter, M4aWriter, encode_adts, encode_adts_with, encode_m4a, encode_m4a_with};

use crate::Error;

/// The sample rates AAC can signal, in Hz: the only rates [`encode_m4a`] and [`encode_adts`] accept.
pub const EXPORT_SAMPLE_RATES: [u32; 13] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];
/// Lowest bitrate per channel, bits per second.
pub const MIN_BITRATE_PER_CHANNEL: u32 = 8_000;
/// Highest bitrate per channel, bits per second, before the frame-size limit of the sample rate.
pub const MAX_BITRATE_PER_CHANNEL: u32 = 320_000;
/// Longest audio accepted, in seconds of input.
pub const MAX_SECONDS: u64 = 86_400;

/// Settings of an AAC encode beyond the audio itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AacOptions {
    /// Total bitrate for all channels, bits per second (constant bitrate).
    pub bitrate_bps: u32,
    /// Text tags for an `.m4a`, as `(key, value)` where the key is a copyright sign and three letters:
    /// `©nam` (title), `©ART` (artist), `©alb`, `©day`, `©cmt`, `©too` (the encoding application).
    /// The application passes its own product name; this crate does not know it. ADTS has no tags, so
    /// [`encode_adts`] ignores them.
    pub metadata: Vec<(String, String)>,
}

impl AacOptions {
    pub fn new(bitrate_bps: u32) -> Self {
        AacOptions { bitrate_bps, metadata: Vec::new() }
    }
}

/// What an encode did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Input frames written. The `.m4a` plays exactly this many.
    pub frames_in: u64,
    /// AAC access units written (`ceil(frames_in / 1024) + 1`: the first carries the priming).
    pub access_units: u64,
    /// NaN or infinite input samples, written as silence.
    pub non_finite: u64,
    /// Finite input samples beyond full scale, clamped.
    pub clipped: u64,
}

/// The lowest and highest total bitrate, bits per second, the encoder takes for this format.
///
/// The ceiling is the lower of [`MAX_BITRATE_PER_CHANNEL`] and the 6144 bits per frame and channel an
/// AAC access unit can hold, which at 1024 samples per frame is 6 times the sample rate.
///
/// # Errors
/// [`Error::InvalidChannels`] unless `channels` is 1 or 2, [`Error::InvalidSampleRate`] unless
/// `sample_rate` is in [`EXPORT_SAMPLE_RATES`].
pub fn bitrate_range(channels: u16, sample_rate: u32) -> Result<(u32, u32), Error> {
    if !(1..=2).contains(&channels) {
        return Err(Error::InvalidChannels(channels));
    }
    if !EXPORT_SAMPLE_RATES.contains(&sample_rate) {
        return Err(Error::InvalidSampleRate(sample_rate));
    }
    let channels = u32::from(channels);
    Ok((MIN_BITRATE_PER_CHANNEL * channels, MAX_BITRATE_PER_CHANNEL.min(6 * sample_rate) * channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_ranges_follow_the_format() {
        assert_eq!(bitrate_range(1, 48_000), Ok((8_000, 288_000)));
        assert_eq!(bitrate_range(2, 48_000), Ok((16_000, 576_000)));
        assert_eq!(bitrate_range(2, 96_000), Ok((16_000, 640_000)));
        assert_eq!(bitrate_range(1, 7_350), Ok((8_000, 44_100)));
        assert_eq!(bitrate_range(0, 48_000), Err(Error::InvalidChannels(0)));
        assert_eq!(bitrate_range(3, 48_000), Err(Error::InvalidChannels(3)));
        assert_eq!(bitrate_range(2, 44_101), Err(Error::InvalidSampleRate(44_101)));
        for rate in EXPORT_SAMPLE_RATES {
            for ch in [1u16, 2] {
                let (min, max) = bitrate_range(ch, rate).unwrap_or((u32::MAX, 0));
                assert!(min < max, "{rate} Hz, {ch} ch");
            }
        }
    }
}
