//! Ogg Opus files from interleaved f32 audio: the one-shot [`encode_opus_ogg`] and the streaming
//! [`OpusOggWriter`].
//!
//! Input is validated before anything is encoded: one or two channels, 8 kHz to 192 kHz, a bitrate
//! inside Opus' range and at most [`MAX_SECONDS`] of audio. NaN and infinite samples become silence and
//! finite ones are clamped to full scale, with a count of each in [`Stats`]. Input that is not 48 kHz is
//! converted first (see `rate_convert`). The file starts at the first input sample: the encoder's
//! delay is the `OpusHead` pre-skip and the final page's granule position trims the padding, so a
//! player reproduces exactly the input length (to the nearest 48 kHz sample).

use std::io::Write;

use crate::ogg::{self, PageWriter};
use crate::rate_convert::Resampler;
use crate::{Application, Error, OpusEncoder};

/// Lowest input sample rate accepted.
pub const MIN_SAMPLE_RATE: u32 = 8_000;
/// Highest input sample rate accepted.
pub const MAX_SAMPLE_RATE: u32 = 192_000;
/// Longest audio accepted, in seconds of input.
pub const MAX_SECONDS: u64 = 86_400;
/// Lowest bitrate per channel, bits per second.
pub const MIN_BITRATE_PER_CHANNEL: u32 = 6_000;
/// Highest total bitrate, bits per second.
pub const MAX_BITRATE: u32 = 510_000;
/// Output rate of every Opus stream.
pub const OPUS_RATE: u32 = 48_000;

/// Frames converted at a time, which bounds the working memory of one `write_interleaved` call.
const CHUNK_FRAMES: usize = 8192;

/// Settings of an Ogg Opus encode beyond the audio itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpusOptions {
    /// Total bitrate for all channels, bits per second.
    pub bitrate_bps: u32,
    /// What the audio is; selects the coding mode and bandwidth for the bitrate.
    pub application: Application,
    /// Encoder name written to the `OpusTags` vendor string. Empty by default: the application passes
    /// its product name, which this crate does not know.
    pub vendor: String,
    /// `KEY=value` comments for `OpusTags` (`TITLE`, `ARTIST`, ...).
    pub comments: Vec<(String, String)>,
    /// Ogg stream serial number. Only matters when streams are chained or multiplexed.
    pub serial: u32,
}

impl OpusOptions {
    pub fn new(bitrate_bps: u32) -> Self {
        OpusOptions { bitrate_bps, application: Application::Audio, vendor: String::new(), comments: Vec::new(), serial: 0x5061_546F }
    }
}

/// What an encode did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Input frames written.
    pub frames_in: u64,
    /// Frames the player reproduces at 48 kHz (the file's duration).
    pub frames_out: u64,
    /// Opus packets written.
    pub packets: u64,
    /// NaN or infinite input samples, written as silence.
    pub non_finite: u64,
    /// Finite input samples beyond full scale, clamped.
    pub clipped: u64,
    /// Opus frames whose range coder had written past the frame's byte budget. Always 0 unless the
    /// encoder's rate control has regressed: such a frame is cut short and decodes as garbage. The donor
    /// asserted on it in debug builds; here it is a count, so release builds and callers can see it too.
    pub frame_overflows: u64,
}

/// Longest input, in frames, at `sample_rate`.
fn max_frames(sample_rate: u32) -> u64 {
    MAX_SECONDS * u64::from(sample_rate)
}

fn check_format(channels: u16, sample_rate: u32, bitrate_bps: u32) -> Result<u8, Error> {
    let ch = u8::try_from(channels).ok().filter(|c| (1..=2).contains(c)).ok_or(Error::InvalidChannels(channels))?;
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Err(Error::InvalidSampleRate(sample_rate));
    }
    let min = MIN_BITRATE_PER_CHANNEL * u32::from(ch);
    if !(min..=MAX_BITRATE).contains(&bitrate_bps) {
        return Err(Error::InvalidBitrate { bitrate: bitrate_bps, min, max: MAX_BITRATE });
    }
    Ok(ch)
}

/// Encodes interleaved `f32` audio (nominally -1.0 to 1.0) to an Ogg Opus file in memory.
///
/// `bitrate_bps` is the total for all channels. See the module documentation for what is validated.
///
/// # Errors
/// Any [`Error`] of [`OpusOggWriter`]; an empty `interleaved` is [`Error::Empty`].
pub fn encode_opus_ogg(interleaved: &[f32], channels: u16, sample_rate: u32, bitrate_bps: u32) -> Result<Vec<u8>, Error> {
    encode_opus_ogg_with(interleaved, channels, sample_rate, &OpusOptions::new(bitrate_bps)).map(|(file, _)| file)
}

/// [`encode_opus_ogg`] with [`OpusOptions`], also returning what the encode did.
pub fn encode_opus_ogg_with(interleaved: &[f32], channels: u16, sample_rate: u32, options: &OpusOptions) -> Result<(Vec<u8>, Stats), Error> {
    check_format(channels, sample_rate, options.bitrate_bps)?;
    // About bitrate * duration / 8, but never trust a size derived from input to reserve memory blindly.
    let seconds = interleaved.len() as u64 / u64::from(channels.max(1)) / u64::from(sample_rate);
    let estimate = (seconds + 1) * u64::from(options.bitrate_bps) / 8 + 4096;
    let sink = Vec::with_capacity(usize::try_from(estimate).unwrap_or(0).min(1 << 26));
    let mut w = OpusOggWriter::new(sink, channels, sample_rate, options)?;
    w.write_interleaved(interleaved)?;
    w.finish()
}

/// Streams interleaved audio into an Ogg Opus file. Memory stays flat however long the audio is: about
/// one second of packets is buffered.
pub struct OpusOggWriter<W: Write> {
    enc: OpusEncoder,
    pages: PageWriter<W>,
    resampler: Option<Resampler>,
    channels: usize,
    in_rate: u32,
    pre_skip: u64,
    /// 48 kHz interleaved frames not yet encoded (less than one packet between calls).
    pending: Vec<f32>,
    packets: u64,
    stats: Stats,
    /// Scratch for sanitised input and converted output.
    scratch: Vec<f32>,
    converted: Vec<f32>,
}

impl<W: Write> OpusOggWriter<W> {
    /// Validates the format, writes the two header pages and returns a writer ready for audio.
    ///
    /// # Errors
    /// [`Error::InvalidChannels`], [`Error::InvalidSampleRate`], [`Error::InvalidBitrate`], an
    /// [`Error::Internal`] for oversized tags, or [`Error::Io`] when the sink refuses the headers.
    pub fn new(out: W, channels: u16, sample_rate: u32, options: &OpusOptions) -> Result<Self, Error> {
        let ch = check_format(channels, sample_rate, options.bitrate_bps)?;
        let enc = OpusEncoder::with_application(ch, options.bitrate_bps, options.application)?;
        let resampler = if sample_rate == OPUS_RATE {
            None
        } else {
            Some(Resampler::new(usize::from(ch), sample_rate, OPUS_RATE).ok_or(Error::Internal("sample-rate converter"))?)
        };
        let pre_skip = enc.pre_skip();
        let head = ogg::opus_head(ch, pre_skip, sample_rate);
        let tags = ogg::opus_tags(&options.vendor, &options.comments)?;
        let mut pages = PageWriter::new(out, options.serial);
        // Each header packet ends its page; the audio starts on a fresh one (RFC 7845 §3).
        pages.packet(&head, 0)?;
        pages.end_page()?;
        pages.packet(&tags, 0)?;
        pages.end_page()?;
        Ok(OpusOggWriter {
            enc,
            pages,
            resampler,
            channels: usize::from(ch),
            in_rate: sample_rate,
            pre_skip: u64::from(pre_skip),
            pending: Vec::new(),
            packets: 0,
            stats: Stats::default(),
            scratch: Vec::new(),
            converted: Vec::new(),
        })
    }

    /// What has been written so far.
    pub fn stats(&self) -> Stats {
        Stats { frame_overflows: self.enc.frame_overflows(), ..self.stats }
    }

    /// Appends interleaved samples.
    ///
    /// # Errors
    /// [`Error::Misaligned`] unless `samples` is a whole number of frames, [`Error::TooLong`] past
    /// [`MAX_SECONDS`] in total, [`Error::Io`] when the sink fails.
    pub fn write_interleaved(&mut self, samples: &[f32]) -> Result<(), Error> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(Error::Misaligned { len: samples.len(), channels: self.channels });
        }
        let frames = (samples.len() / self.channels) as u64;
        if self.stats.frames_in.checked_add(frames).is_none_or(|total| total > max_frames(self.in_rate)) {
            return Err(Error::TooLong);
        }
        for chunk in samples.chunks(CHUNK_FRAMES * self.channels) {
            self.scratch.clear();
            self.scratch.extend(chunk.iter().map(|&v| sanitise(v, &mut self.stats)));
            self.stats.frames_in += (chunk.len() / self.channels) as u64;
            match self.resampler.as_mut() {
                Some(r) => {
                    self.converted.clear();
                    r.process(&self.scratch, u64::MAX, &mut self.converted);
                    self.pending.extend_from_slice(&self.converted);
                }
                None => self.pending.extend_from_slice(&self.scratch),
            }
            self.encode_full_packets()?;
        }
        Ok(())
    }

    /// Writes the final packets and page and returns the sink with the totals.
    ///
    /// # Errors
    /// [`Error::Empty`] when no audio was written; [`Error::Io`] when the sink fails.
    pub fn finish(mut self) -> Result<(W, Stats), Error> {
        if self.stats.frames_in == 0 {
            return Err(Error::Empty);
        }
        let total_out = match self.resampler.as_mut() {
            Some(r) => {
                let total = r.output_len(self.stats.frames_in);
                self.converted.clear();
                r.finish(total, &mut self.converted);
                self.pending.extend_from_slice(&self.converted);
                total
            }
            None => self.stats.frames_in,
        };
        self.stats.frames_out = total_out;
        // The decoder drops `pre_skip` samples from the start, so the stream must carry the signal for
        // pre_skip + total_out samples; the rest of the last packet is zeros the final granule trims.
        let wanted = total_out + self.pre_skip;
        let packets = wanted.div_ceil(crate::N as u64);
        let frame = crate::N * self.channels;
        let final_granule = i64::try_from(wanted).map_err(|_| Error::TooLong)?;
        while self.packets < packets {
            if self.pending.len() < frame {
                self.pending.resize(frame, 0.0);
            }
            let rest = self.pending.split_off(frame);
            let block = std::mem::replace(&mut self.pending, rest);
            let packet = self.enc.encode_float(&block)?;
            self.packets += 1;
            let granule = if self.packets == packets { final_granule } else { granule_after(self.packets)? };
            self.pages.packet(&packet, granule)?;
        }
        self.stats.packets = self.packets;
        self.stats.frame_overflows = self.enc.frame_overflows();
        let out = self.pages.finish()?;
        Ok((out, self.stats))
    }

    /// Encodes every complete 20 ms block in `pending`.
    fn encode_full_packets(&mut self) -> Result<(), Error> {
        let frame = crate::N * self.channels;
        let full = self.pending.len() / frame;
        for block in self.pending.chunks_exact(frame).take(full) {
            let packet = self.enc.encode_float(block)?;
            self.packets += 1;
            self.pages.packet(&packet, granule_after(self.packets)?)?;
        }
        self.pending.drain(..full * frame);
        Ok(())
    }
}

/// Granule position (RFC 7845 §4) after `packets` whole packets: the samples the decoder has output by
/// then, counting the ones it will discard as pre-skip.
fn granule_after(packets: u64) -> Result<i64, Error> {
    packets.checked_mul(crate::N as u64).and_then(|g| i64::try_from(g).ok()).ok_or(Error::TooLong)
}

/// NaN and infinities become silence; finite values are clamped to full scale.
fn sanitise(v: f32, stats: &mut Stats) -> f32 {
    if !v.is_finite() {
        stats.non_finite += 1;
        0.0
    } else if v.abs() > 1.0 {
        stats.clipped += 1;
        v.clamp(-1.0, 1.0)
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_checks() {
        assert_eq!(check_format(2, 48_000, 128_000), Ok(2));
        assert_eq!(check_format(0, 48_000, 128_000), Err(Error::InvalidChannels(0)));
        assert_eq!(check_format(3, 48_000, 128_000), Err(Error::InvalidChannels(3)));
        assert_eq!(check_format(65_535, 48_000, 128_000), Err(Error::InvalidChannels(65_535)));
        assert_eq!(check_format(2, 7_999, 128_000), Err(Error::InvalidSampleRate(7_999)));
        assert_eq!(check_format(2, 192_001, 128_000), Err(Error::InvalidSampleRate(192_001)));
        assert_eq!(check_format(2, 0, 128_000), Err(Error::InvalidSampleRate(0)));
        assert_eq!(check_format(2, u32::MAX, 128_000), Err(Error::InvalidSampleRate(u32::MAX)));
        assert!(check_format(2, 48_000, 11_999).is_err());
        assert!(check_format(1, 48_000, 11_999).is_ok());
        assert!(check_format(1, 48_000, 5_999).is_err());
        assert!(check_format(2, 48_000, 510_001).is_err());
        assert!(check_format(2, 48_000, 510_000).is_ok());
        assert!(check_format(2, 48_000, 0).is_err());
        assert!(check_format(2, 48_000, u32::MAX).is_err());
    }

    #[test]
    fn the_duration_bound_does_not_overflow() {
        assert_eq!(max_frames(MAX_SAMPLE_RATE), 86_400 * 192_000);
        assert!(u64::MAX.checked_add(1).is_none_or(|t| t > max_frames(48_000)));
    }

    #[test]
    fn sanitising_counts_and_clamps() {
        let mut s = Stats::default();
        assert_eq!(sanitise(f32::NAN, &mut s), 0.0);
        assert_eq!(sanitise(f32::INFINITY, &mut s), 0.0);
        assert_eq!(sanitise(f32::NEG_INFINITY, &mut s), 0.0);
        assert_eq!(sanitise(3.0, &mut s), 1.0);
        assert_eq!(sanitise(-1e30, &mut s), -1.0);
        assert_eq!(sanitise(0.5, &mut s), 0.5);
        assert_eq!(sanitise(f32::MIN_POSITIVE / 2.0, &mut s), f32::MIN_POSITIVE / 2.0);
        assert_eq!((s.non_finite, s.clipped), (3, 2));
    }
}
