//! The `aac-encode` build: validation, the encoder and the two containers.

use std::io::{Cursor, Read, Seek, Write};

use soundcraft_isobmff::{Brand, CodecConfig, Edit, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

use super::{AacOptions, MAX_SECONDS, Stats, bitrate_range};
use crate::{Encoder, EncoderConfig, Error, FRAME_LEN};

/// Frames converted at a time, which bounds the working memory of one `write_interleaved` call.
const CHUNK_FRAMES: usize = 8192;
/// Most tags accepted in an `.m4a`.
const MAX_TAGS: usize = 32;
/// Longest tag value accepted, in bytes.
const MAX_TAG_BYTES: usize = 1024;
/// `Edit::media_rate` for normal speed (16.16 fixed point).
const NORMAL_SPEED: i32 = 0x1_0000;

/// Encodes interleaved `f32` audio (nominally -1.0 to 1.0) to an `.m4a` file in memory, with the
/// `moov` box in front of the audio so that it can play while downloading.
///
/// `bitrate_bps` is the total for all channels. See the module documentation for what is validated.
///
/// # Errors
/// Any [`Error`] of [`M4aWriter`]; an empty `interleaved` is [`Error::Empty`].
pub fn encode_m4a(interleaved: &[f32], channels: u16, sample_rate: u32, bitrate_bps: u32) -> Result<Vec<u8>, Error> {
    encode_m4a_with(interleaved, channels, sample_rate, &AacOptions::new(bitrate_bps)).map(|(file, _)| file)
}

/// [`encode_m4a`] with [`AacOptions`], also returning what the encode did.
pub fn encode_m4a_with(interleaved: &[f32], channels: u16, sample_rate: u32, options: &AacOptions) -> Result<(Vec<u8>, Stats), Error> {
    let sink = Cursor::new(Vec::with_capacity(size_estimate(interleaved.len(), channels, sample_rate, options.bitrate_bps)));
    let mut w = M4aWriter::new(sink, channels, sample_rate, options)?;
    w.write_interleaved(interleaved)?;
    let (sink, stats) = w.finish_faststart()?;
    Ok((sink.into_inner(), stats))
}

/// Encodes interleaved `f32` audio to a raw ADTS stream (`.aac`) in memory. The stream carries no
/// delay or length information; see the module documentation.
///
/// # Errors
/// Any [`Error`] of [`AdtsWriter`]; an empty `interleaved` is [`Error::Empty`].
pub fn encode_adts(interleaved: &[f32], channels: u16, sample_rate: u32, bitrate_bps: u32) -> Result<Vec<u8>, Error> {
    encode_adts_with(interleaved, channels, sample_rate, &AacOptions::new(bitrate_bps)).map(|(file, _)| file)
}

/// [`encode_adts`] with [`AacOptions`], also returning what the encode did.
pub fn encode_adts_with(interleaved: &[f32], channels: u16, sample_rate: u32, options: &AacOptions) -> Result<(Vec<u8>, Stats), Error> {
    let sink = Vec::with_capacity(size_estimate(interleaved.len(), channels, sample_rate, options.bitrate_bps));
    let mut w = AdtsWriter::new(sink, channels, sample_rate, options)?;
    w.write_interleaved(interleaved)?;
    w.finish()
}

/// About `bitrate * duration / 8` bytes, capped: a size derived from input must not reserve memory blindly.
fn size_estimate(samples: usize, channels: u16, sample_rate: u32, bitrate_bps: u32) -> usize {
    let seconds = samples as u64 / u64::from(channels.max(1)) / u64::from(sample_rate.max(1));
    let bytes = (seconds + 1).saturating_mul(u64::from(bitrate_bps)) / 8 + 4096;
    usize::try_from(bytes).unwrap_or(0).min(1 << 26)
}

/// Streams interleaved audio into an `.m4a` file.
///
/// The media data is written as it is encoded, so memory stays flat however long the audio is, and the
/// `moov` box is written by [`finish`](Self::finish) (or [`finish_faststart`](Self::finish_faststart)).
/// A writer that is dropped without `finish` leaves a file no player can open.
pub struct M4aWriter<W: Write + Seek> {
    mux: Mp4Writer<W>,
    track: usize,
    core: Core,
}

impl<W: Write + Seek> M4aWriter<W> {
    /// Validates the format and writes the file header.
    ///
    /// # Errors
    /// [`Error::InvalidChannels`], [`Error::InvalidSampleRate`], [`Error::InvalidBitrate`],
    /// [`Error::InvalidMetadata`], or [`Error::Io`] when the sink refuses the header.
    pub fn new(out: W, channels: u16, sample_rate: u32, options: &AacOptions) -> Result<Self, Error> {
        let core = Core::new(channels, sample_rate, options.bitrate_bps, false)?;
        let metadata = check_metadata(&options.metadata)?;
        let mut entry = SampleEntry::aac(core.enc.audio_specific_config(), u32::from(channels), sample_rate);
        if let CodecConfig::Aac(cfg) = &mut entry.codec {
            cfg.avg_bitrate = options.bitrate_bps;
            cfg.max_bitrate = options.bitrate_bps;
            // the decoder buffer: the most one frame can hold (6144 bits per channel)
            cfg.buffer_size = 768 * u32::from(channels);
        }
        // One tick of the movie clock is one audio sample, so the edit list below is exact.
        let mut opts = WriterOptions::new(Brand::M4a);
        opts.movie_timescale = sample_rate;
        opts.metadata = metadata;
        let mut mux = Mp4Writer::new(out, opts).map_err(container_error)?;
        let mut track = TrackConfig::new(entry, sample_rate);
        // Provisional edit (priming only); `finish` replaces it with the exact one.
        track.media_start = Some(i64::from(core.enc.priming_samples()));
        let track = mux.add_track(track).map_err(container_error)?;
        Ok(M4aWriter { mux, track, core })
    }

    /// What has been written so far.
    pub fn stats(&self) -> Stats {
        self.core.stats
    }

    /// Appends interleaved samples.
    ///
    /// # Errors
    /// [`Error::Misaligned`] unless `samples` is a whole number of frames, [`Error::TooLong`] past
    /// [`MAX_SECONDS`] in total, [`Error::Io`] when the sink fails.
    pub fn write_interleaved(&mut self, samples: &[f32]) -> Result<(), Error> {
        let Self { mux, track, core } = self;
        core.push(samples, |au| write_access_unit(mux, *track, au))
    }

    /// Writes the last access units and the `moov` box and returns the sink with the totals.
    ///
    /// # Errors
    /// [`Error::Empty`] when no audio was written; [`Error::Io`] when the sink fails.
    pub fn finish(self) -> Result<(W, Stats), Error> {
        let (mux, stats) = self.close()?;
        Ok((mux.finish().map_err(container_error)?, stats))
    }

    /// Everything up to the `moov` box: the last access units and the exact edit list.
    fn close(self) -> Result<(Mp4Writer<W>, Stats), Error> {
        let Self { mut mux, track, mut core } = self;
        if core.stats.frames_in == 0 {
            return Err(Error::Empty);
        }
        core.finish(|au| write_access_unit(&mut mux, track, au))?;
        // Decoding gives priming + ceil(n / 1024) * 1024 samples; the player skips the priming and
        // stops after exactly the `n` input frames.
        let edit = Edit { segment_duration: core.stats.frames_in, media_time: i64::from(core.enc.priming_samples()), media_rate: NORMAL_SPEED };
        mux.set_edits(track, vec![edit]).map_err(container_error)?;
        Ok((mux, core.stats))
    }
}

impl<W: Read + Write + Seek> M4aWriter<W> {
    /// [`finish`](Self::finish) with the `moov` box moved in front of the audio ("fast start"), which
    /// needs a sink that can also be read back, like a file opened for reading and writing.
    ///
    /// # Errors
    /// As [`finish`](Self::finish).
    pub fn finish_faststart(self) -> Result<(W, Stats), Error> {
        let (mux, stats) = self.close()?;
        Ok((mux.finish_faststart().map_err(container_error)?, stats))
    }
}

/// Streams interleaved audio into a raw ADTS stream (`.aac`): one 7-byte header and one access unit per
/// 1024 frames.
pub struct AdtsWriter<W: Write> {
    out: W,
    core: Core,
}

impl<W: Write> AdtsWriter<W> {
    /// Validates the format. `options.metadata` is ignored: ADTS has no tags.
    ///
    /// # Errors
    /// [`Error::InvalidChannels`], [`Error::InvalidSampleRate`] or [`Error::InvalidBitrate`].
    pub fn new(out: W, channels: u16, sample_rate: u32, options: &AacOptions) -> Result<Self, Error> {
        Ok(AdtsWriter { out, core: Core::new(channels, sample_rate, options.bitrate_bps, true)? })
    }

    /// What has been written so far.
    pub fn stats(&self) -> Stats {
        self.core.stats
    }

    /// Appends interleaved samples.
    ///
    /// # Errors
    /// [`Error::Misaligned`] unless `samples` is a whole number of frames, [`Error::TooLong`] past
    /// [`MAX_SECONDS`] in total, [`Error::Io`] when the sink fails.
    pub fn write_interleaved(&mut self, samples: &[f32]) -> Result<(), Error> {
        let Self { out, core } = self;
        core.push(samples, |au| out.write_all(au).map_err(|e| Error::Io(e.to_string())))
    }

    /// Writes the last frames and returns the sink with the totals.
    ///
    /// # Errors
    /// [`Error::Empty`] when no audio was written; [`Error::Io`] when the sink fails.
    pub fn finish(self) -> Result<(W, Stats), Error> {
        let Self { mut out, mut core } = self;
        if core.stats.frames_in == 0 {
            return Err(Error::Empty);
        }
        core.finish(|au| out.write_all(au).map_err(|e| Error::Io(e.to_string())))?;
        out.flush().map_err(|e| Error::Io(e.to_string()))?;
        Ok((out, core.stats))
    }
}

/// Validation, sanitising and the encoder, shared by both containers.
struct Core {
    enc: Encoder,
    channels: usize,
    /// Most frames accepted in total.
    max_frames: u64,
    stats: Stats,
    /// Planar scratch for one chunk.
    planar: Vec<Vec<f32>>,
}

impl Core {
    fn new(channels: u16, sample_rate: u32, bitrate_bps: u32, adts: bool) -> Result<Core, Error> {
        let (min, max) = bitrate_range(channels, sample_rate)?;
        if !(min..=max).contains(&bitrate_bps) {
            return Err(Error::InvalidBitrate { bitrate: bitrate_bps, min, max });
        }
        let channels = usize::from(channels);
        let mut config = EncoderConfig::cbr(sample_rate, channels, bitrate_bps);
        config.adts = adts;
        Ok(Core {
            enc: Encoder::new(config)?,
            channels,
            max_frames: MAX_SECONDS * u64::from(sample_rate),
            stats: Stats::default(),
            planar: vec![Vec::new(); channels],
        })
    }

    /// Sanitises `samples`, encodes them and hands every finished access unit to `sink`.
    fn push(&mut self, samples: &[f32], mut sink: impl FnMut(&[u8]) -> Result<(), Error>) -> Result<(), Error> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(Error::Misaligned { len: samples.len(), channels: self.channels });
        }
        let frames = (samples.len() / self.channels) as u64;
        if self.stats.frames_in.checked_add(frames).is_none_or(|total| total > self.max_frames) {
            return Err(Error::TooLong);
        }
        for chunk in samples.chunks(CHUNK_FRAMES * self.channels) {
            for plane in &mut self.planar {
                plane.clear();
            }
            for frame in chunk.chunks_exact(self.channels) {
                for (plane, &v) in self.planar.iter_mut().zip(frame) {
                    plane.push(sanitise(v, &mut self.stats));
                }
            }
            self.stats.frames_in += (chunk.len() / self.channels) as u64;
            let planes: Vec<&[f32]> = self.planar.iter().map(Vec::as_slice).collect();
            for au in self.enc.encode(&planes) {
                self.stats.access_units += 1;
                sink(&au)?;
            }
        }
        Ok(())
    }

    /// Encodes the tail (zero padded) and hands the last access units to `sink`.
    fn finish(&mut self, mut sink: impl FnMut(&[u8]) -> Result<(), Error>) -> Result<(), Error> {
        for au in self.enc.flush() {
            self.stats.access_units += 1;
            sink(&au)?;
        }
        Ok(())
    }
}

fn write_access_unit<W: Write + Seek>(mux: &mut Mp4Writer<W>, track: usize, au: &[u8]) -> Result<(), Error> {
    mux.write_sample(track, WriteSample { data: au, duration: FRAME_LEN as u32, composition_offset: 0, is_sync: true }).map_err(container_error)
}

fn container_error(e: soundcraft_isobmff::Error) -> Error {
    match e {
        soundcraft_isobmff::Error::Io(e) => Error::Io(e.to_string()),
        other => Error::Container(other.to_string()),
    }
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

/// Keys must be a copyright sign and three printable ASCII letters (the `©xxx` atoms of the movie
/// header); values are bounded.
fn check_metadata(tags: &[(String, String)]) -> Result<Vec<(String, String)>, Error> {
    if tags.len() > MAX_TAGS {
        return Err(Error::InvalidMetadata("too many tags"));
    }
    for (key, value) in tags {
        let mut chars = key.chars();
        let key_ok = chars.next() == Some('\u{a9}') && chars.clone().count() == 3 && chars.all(|c| c.is_ascii_graphic());
        if !key_ok {
            return Err(Error::InvalidMetadata("a tag key is a copyright sign and three letters"));
        }
        if value.len() > MAX_TAG_BYTES {
            return Err(Error::InvalidMetadata("a tag value is too long"));
        }
    }
    Ok(tags.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EXPORT_SAMPLE_RATES;

    #[test]
    fn export_rates_are_the_codec_rates() {
        assert_eq!(EXPORT_SAMPLE_RATES, crate::SAMPLE_RATES);
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
        assert_eq!((s.non_finite, s.clipped), (3, 2));
    }

    #[test]
    fn metadata_keys_and_values_are_checked() {
        let tag = |k: &str, v: &str| vec![(k.to_string(), v.to_string())];
        assert!(check_metadata(&tag("\u{a9}nam", "Title")).is_ok());
        assert!(check_metadata(&tag("\u{a9}ART", "")).is_ok());
        assert!(check_metadata(&[]).is_ok());
        for bad in ["", "nam", "\u{a9}na", "\u{a9}name", "\u{a9}n m", "\u{a9}n\u{e9}m", "xnam"] {
            assert!(check_metadata(&tag(bad, "x")).is_err(), "{bad:?}");
        }
        assert!(check_metadata(&tag("\u{a9}nam", &"x".repeat(MAX_TAG_BYTES))).is_ok());
        assert!(check_metadata(&tag("\u{a9}nam", &"x".repeat(MAX_TAG_BYTES + 1))).is_err());
        assert!(check_metadata(&vec![("\u{a9}nam".to_string(), "x".to_string()); MAX_TAGS + 1]).is_err());
    }

    #[test]
    fn too_long_is_refused_before_anything_is_encoded() {
        let mut core = Core::new(2, 48_000, 128_000, false).unwrap();
        core.max_frames = 3000;
        let mut units = 0;
        assert!(core.push(&[0.1; 2 * 3000], |_| Ok(units += 1)).is_ok());
        let before = core.stats;
        assert_eq!(core.push(&[0.1; 2], |_| Ok(())), Err(Error::TooLong));
        assert_eq!(core.stats, before, "a refused call changes nothing");
        // a count that would overflow u64 is refused too, not wrapped
        core.stats.frames_in = u64::MAX;
        assert_eq!(core.push(&[0.1; 2], |_| Ok(())), Err(Error::TooLong));
        // an empty call at the limit is fine
        core.stats.frames_in = 3000;
        assert_eq!(core.push(&[], |_| Ok(())), Ok(()));
    }

    #[test]
    fn the_duration_bound_does_not_overflow() {
        assert!(u64::MAX.checked_add(1).is_none_or(|t| t > MAX_SECONDS * 96_000));
        assert_eq!(size_estimate(usize::MAX, 1, 7_350, u32::MAX), 1 << 26);
        assert_eq!(size_estimate(0, 0, 0, 0), 4096);
    }
}
