//! AV1 as a [`GopDecoder`]. The decoder is the compile-time feature `av1-decode` (off by default);
//! without it every AV1 track opens as "unsupported in this build", so the timeline still shows the
//! clip and frame requests say why. With the feature on, [`crate::av1`] decodes every profile
//! (Main, High, Professional), 8, 10 and 12 bits, 4:2:0, 4:2:2, 4:4:4 and monochrome.

use crate::gop::{Described, GopDecoder};
use crate::isobmff::Av1Config;
use crate::{Result, VideoError};

#[cfg(feature = "av1-decode")]
use crate::convert::{Planes, Yuv};
#[cfg(feature = "av1-decode")]
use crate::gop::Pic;

/// What the `av1C` record says about decodability (profile, bit depth, chroma format).
#[cfg(not(feature = "av1-decode"))]
pub(crate) fn describe(_cfg: &Av1Config) -> Described {
    Described::not_in_build("AV1")
}

/// A decoder primed with the sequence header OBUs from `av1C`.
#[cfg(not(feature = "av1-decode"))]
pub(crate) fn open(_config_obus: &[u8]) -> Result<Box<dyn GopDecoder>> {
    Err(VideoError::Unsupported("AV1 pictures are unsupported in this build".into()))
}

/// What the `av1C` record says about decodability (profile, bit depth, chroma format). The record
/// is only a hint (the sequence header in the stream decides), so anything that names a real
/// profile and chroma layout is accepted.
#[cfg(feature = "av1-decode")]
pub(crate) fn describe(cfg: &Av1Config) -> Described {
    let profile = match cfg.seq_profile {
        0 => "Main",
        1 => "High",
        2 => "Professional",
        p => {
            return Described { detail: "AV1".into(), unsupported: Some(format!("AV1 profile {p} is not supported")) };
        }
    };
    let depth = match (cfg.high_bitdepth, cfg.twelve_bit) {
        (false, _) => 8,
        (true, false) => 10,
        (true, true) => 12,
    };
    let chroma = match (cfg.monochrome, cfg.chroma_subsampling_x, cfg.chroma_subsampling_y) {
        (true, ..) => "monochrome",
        (false, true, true) => "4:2:0",
        (false, true, false) => "4:2:2",
        (false, false, false) => "4:4:4",
        (false, false, true) => {
            return Described { detail: "AV1".into(), unsupported: Some("AV1 with this chroma layout is not supported".into()) };
        }
    };
    // The sequence header the record carries says how large frames get. A picture whose references and
    // working copies cannot fit the decoder's memory budget (1.6 GiB, Gil's decision 5b) would open as
    // decodable and then fail on every frame, so it is refused here, up front, the way HEVC is.
    //
    // The size checked is the sequence header's DECLARED MAXIMUM (`max_frame_width` x `max_frame_height`),
    // not the size of any frame coded in the stream: this runs when the file opens, before one frame has
    // been read, and Gil wants a refusal before anything is allocated. So a stream that declares an 8K
    // ceiling but codes smaller frames is refused here too, although a `Decoder` fed such a stream
    // directly would decode it (its own check at each frame header uses the coded size). Encoders write
    // the ceiling they mean to use, so this is the size the file will need. `docs/video-codecs.md`
    // ("Limits") says the same.
    let unsupported = crate::av1::find_sequence_header(&cfg.config_obus).and_then(|seq| {
        let (w, h) = (seq.max_frame_width, seq.max_frame_height);
        let fits = |budget| crate::av1::picture_fits_budget(&seq, w, h, budget);
        (!fits(crate::av1::DEFAULT_MEMORY_BUDGET)).then(|| {
            // beyond the format's own limits (level 6.3, 8192 x 8192) whatever the memory, or only the budget?
            let why = if fits(u64::MAX) { "picture too large for the memory budget" } else { "picture size beyond the decoder's limits" };
            format!("AV1 {w}x{h} pictures are unsupported in this build: {why}")
        })
    });
    Described { detail: format!("AV1 {profile} {depth}-bit {chroma}"), unsupported }
}

/// A decoder primed with the sequence header OBUs from `av1C`.
#[cfg(feature = "av1-decode")]
pub(crate) fn open(config_obus: &[u8]) -> Result<Box<dyn GopDecoder>> {
    // One thread: Movie decodes on its caller's thread and asks for one picture at a time, and a
    // decoder per thumbnail must not build a thread pool.
    let mut dec = crate::av1::Decoder::with_threads(1);
    if !config_obus.is_empty() {
        // The record repeats the sequence header that key frames carry in-band. A record that is
        // damaged or missing is not fatal: the first key frame supplies the header, and if it does
        // not either, that frame's own error says so.
        let _ = dec.decode_pts(config_obus, 0);
        let _ = dec.flush();
    }
    Ok(Box::new(Av1Gop { dec }))
}

#[cfg(feature = "av1-decode")]
struct Av1Gop {
    dec: crate::av1::Decoder,
}

/// `Unsupported` stops the movie's decode; everything else is a damaged access unit to skip.
#[cfg(feature = "av1-decode")]
fn map_error(e: crate::av1::Error) -> VideoError {
    match e {
        crate::av1::Error::Unsupported(what) => VideoError::Unsupported(format!("AV1: {what}")),
        other => VideoError::Decode(other.to_string()),
    }
}

/// Narrow 8-bit-depth samples to bytes (values above 255 cannot occur; they clamp).
#[cfg(feature = "av1-decode")]
fn narrow(plane: &[u16]) -> Vec<u8> {
    plane.iter().map(|&v| v.min(255) as u8).collect()
}

/// Identity matrix coefficients (H.273 code 0) store G, B and R in the Y, U and V planes. The
/// movie's converter reads Y'CbCr, so rebuild full-range BT.709 Y'CbCr from the RGB samples.
#[cfg(feature = "av1-decode")]
fn gbr_to_ycbcr(g: &[u16], b: &[u16], r: &[u16], bit_depth: u8, full_range: bool) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let bits = u32::from(bit_depth.clamp(8, 16));
    let max = (1i64 << bits) - 1;
    let half = 1i64 << (bits - 1);
    let (black, span) = if full_range { (0i64, max) } else { (16i64 << (bits - 8), 219i64 << (bits - 8)) };
    // RGB code -> 0..=max full range.
    let norm = |v: u16| ((i64::from(v) - black) * max + span / 2).div_euclid(span).clamp(0, max);
    let n = g.len().min(b.len()).min(r.len());
    let (mut yy, mut cb, mut cr) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        let (gg, bb, rr) = (norm(g[i]), norm(b[i]), norm(r[i]));
        let y = (13933 * rr + 46871 * gg + 4732 * bb + 32768) >> 16;
        yy.push(y.clamp(0, max) as u16);
        cb.push((half + (((bb - y) * 35318 + 32768) >> 16)).clamp(0, max) as u16);
        cr.push((half + (((rr - y) * 41615 + 32768) >> 16)).clamp(0, max) as u16);
    }
    (yy, cb, cr)
}

/// A decoded picture as the movie's planar picture, refusing sizes and planes that do not add up.
#[cfg(feature = "av1-decode")]
fn pic(p: crate::av1::Picture) -> Result<Pic> {
    let (w, h) = (p.width, p.height);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > crate::movie::MAX_PIXELS {
        return Err(VideoError::Decode(format!("AV1 picture of {w}x{h} samples")));
    }
    let (cw, ch) = (p.plane_width(1), p.plane_height(1));
    let luma = w as usize * h as usize;
    let chroma = cw as usize * ch as usize;
    let mono = p.mono_chrome;
    let [y, u, v] = p.planes;
    if y.len() != luma || (!mono && (u.len() != chroma || v.len() != chroma)) || (mono && (!u.is_empty() || !v.is_empty())) {
        return Err(VideoError::Decode("AV1 picture planes do not match its size".into()));
    }
    let (cx, cy) = (if p.subsampling_x == 1 { 2 } else { 1 }, if p.subsampling_y == 1 { 2 } else { 1 });
    // Identity coefficients are RGB (4:4:4 only); turn them into Y'CbCr for the converter.
    let identity = p.matrix_coefficients == 0 && !mono && cx == 1 && cy == 1;
    let (planes, matrix, full_range) = if identity {
        let (y, u, v) = gbr_to_ycbcr(&y, &u, &v, p.bit_depth, p.full_range);
        (Planes::U16 { y, u, v }, 1, true)
    } else if p.bit_depth == 8 {
        (Planes::U8 { y: narrow(&y), u: narrow(&u), v: narrow(&v) }, p.matrix_coefficients, p.full_range)
    } else {
        (Planes::U16 { y, u, v }, p.matrix_coefficients, p.full_range)
    };
    let yuv = Yuv { width: w, height: h, planes, y_stride: w as usize, uv_stride: cw as usize, cx, cy, bit_depth: p.bit_depth, matrix, full_range };
    Ok(Pic { pts: p.pts, yuv })
}

#[cfg(feature = "av1-decode")]
impl GopDecoder for Av1Gop {
    fn decode(&mut self, au: &[u8], pts: i64) -> Result<Vec<Pic>> {
        let pictures = self.dec.decode_pts(au, pts).map_err(map_error)?;
        pictures.into_iter().map(pic).collect()
    }

    fn flush(&mut self) -> Vec<Pic> {
        self.dec.flush().into_iter().filter_map(|p| pic(p).ok()).collect()
    }

    /// The container's sync flag is not trusted: some muxers flag every key frame, including ones
    /// that are not shown on their own, and a stream can start decoding only at a shown key frame.
    fn checks_random_access(&self) -> bool {
        true
    }

    fn is_random_access(&self, au: &[u8]) -> bool {
        // A reduced still-picture sequence has no frame-type bits: every frame is a key frame.
        let still = self.dec.sequence_header().is_some_and(|s| s.reduced_still_picture_header);
        crate::av1::is_key_frame_unit(au) || (still && crate::av1::has_frame_unit(au))
    }
}

#[cfg(all(test, feature = "av1-decode"))]
mod tests {
    use super::*;
    use crate::av1::Picture;

    const IVF: &[u8] = include_bytes!("../tests/fixtures/av1/av1-8bit-64x64-4f.ivf");

    /// The temporal units of an IVF file.
    fn ivf_frames(data: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut pos = 32;
        while let Some(h) = data.get(pos..pos + 12) {
            let size = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
            pos += 12;
            let Some(frame) = data.get(pos..pos + size) else { break };
            out.push(frame.to_vec());
            pos += size;
        }
        out
    }

    fn config(profile: u8, high: bool, twelve: bool, mono: bool, ssx: bool, ssy: bool) -> Av1Config {
        Av1Config {
            seq_profile: profile,
            high_bitdepth: high,
            twelve_bit: twelve,
            monochrome: mono,
            chroma_subsampling_x: ssx,
            chroma_subsampling_y: ssy,
            ..Av1Config::default()
        }
    }

    #[test]
    fn describe_names_profile_depth_and_chroma_without_third_party_names() {
        let cases = [
            (config(0, false, false, false, true, true), "AV1 Main 8-bit 4:2:0"),
            (config(0, true, false, false, true, true), "AV1 Main 10-bit 4:2:0"),
            (config(0, false, false, true, true, true), "AV1 Main 8-bit monochrome"),
            (config(1, false, false, false, false, false), "AV1 High 8-bit 4:4:4"),
            (config(2, true, true, false, true, false), "AV1 Professional 12-bit 4:2:2"),
        ];
        for (cfg, want) in cases {
            let d = describe(&cfg);
            assert_eq!((d.detail.as_str(), d.unsupported), (want, None));
            let lower = want.to_ascii_lowercase();
            assert!(!["apple", "prores", "avid", "dnx"].iter().any(|w| lower.contains(w)));
        }
        for bad in [config(3, false, false, false, true, true), config(0, false, false, false, false, true)] {
            let d = describe(&bad);
            assert!(d.unsupported.is_some_and(|w| w.contains("not supported")), "{bad:?}");
            assert_eq!(d.detail, "AV1");
        }
    }

    /// The chroma layouts the sequence header builder below writes (each needs its own profile).
    #[derive(Clone, Copy)]
    enum Chroma {
        C420,
        C422,
        C444,
    }

    /// The sequence header OBU of a reduced still-picture sequence of `w` x `h`, 8 or 10 bits
    /// (`high`), with or without film grain.
    fn sequence_obu_of(w: u32, h: u32, chroma: Chroma, high: bool, grain: bool) -> Vec<u8> {
        let profile = match chroma {
            Chroma::C420 => 0,
            Chroma::C444 => 1,
            Chroma::C422 => 2,
        };
        let mut b = crate::bitstream::BitWriter::new();
        b.write_bits(profile, 3); // seq_profile
        b.write_bits(1, 1); // still_picture
        b.write_bits(1, 1); // reduced_still_picture_header
        b.write_bits(0, 5); // seq_level_idx
        b.write_bits(15, 4); // frame_width_bits_minus_1
        b.write_bits(15, 4); // frame_height_bits_minus_1
        b.write_bits(w - 1, 16);
        b.write_bits(h - 1, 16);
        b.write_bits(0, 3 + 3); // superblock size, filter intra, edge filter; superres, cdef, restoration
        b.write_bits(u32::from(high), 1); // color_config: high_bitdepth
        if profile == 2 && high {
            b.write_bits(0, 1); // twelve_bit
        }
        if profile != 1 {
            b.write_bits(0, 1); // mono_chrome
        }
        b.write_bits(0, 1); // color_description_present_flag
        b.write_bits(0, 1); // color_range
        if profile == 0 {
            b.write_bits(0, 2); // chroma_sample_position
        }
        b.write_bits(0, 1); // separate_uv_delta_q
        b.write_bits(u32::from(grain), 1); // film_grain_params_present
        b.rbsp_trailing();
        let payload = b.finish();
        let mut obu = vec![1 << 3 | 0b10, payload.len() as u8];
        obu.extend(payload);
        obu
    }

    /// The sequence header OBU of a reduced still-picture sequence of `w` x `h` (4:2:0, 8-bit).
    fn sequence_obu(w: u32, h: u32) -> Vec<u8> {
        sequence_obu_of(w, h, Chroma::C420, false, false)
    }

    /// The `av1C` record for a sequence of `w` x `h` in `chroma`, 8 or 10 bits, with or without film grain.
    fn record(w: u32, h: u32, chroma: Chroma, high: bool, grain: bool) -> Av1Config {
        let (profile, ssx, ssy) = match chroma {
            Chroma::C420 => (0, true, true),
            Chroma::C422 => (2, true, false),
            Chroma::C444 => (1, false, false),
        };
        Av1Config { config_obus: sequence_obu_of(w, h, chroma, high, grain), ..config(profile, high, false, false, ssx, ssy) }
    }

    /// Gil's decision 5b (V4): a 1.6 GiB decoder budget. An AV1 picture the decoder would refuse on every frame
    /// is refused when the file opens, with the reason, rather than shown as decodable.
    #[test]
    fn a_picture_beyond_the_memory_budget_is_refused_when_the_file_opens() {
        let base = config(0, false, false, false, true, true);
        for (w, h, fits, reason) in [
            (64, 64, true, ""),
            (1920, 1080, true, ""),
            (3840, 2160, true, ""),
            (5120, 2880, true, ""),
            (7424, 4176, true, ""),
            (7680, 4320, true, ""), // full 8K UHD: 1.594 GiB estimated, under the 1.6 GiB budget
            (7680, 4352, true, ""),
            (8192, 4096, false, "too large for the memory budget"), // DCI 8K: 3.8 MB over
            (8192, 4320, false, "too large for the memory budget"),
            (8192, 4352, false, "too large for the memory budget"), // the level 6.3 maximum: only the budget stops it
            (8192, 8192, false, "size beyond the decoder's limits"), // past level 6.3 and the 8192 x 8192 area: no budget would do
        ] {
            let d = describe(&Av1Config { config_obus: sequence_obu(w, h), ..base.clone() });
            assert_eq!(d.detail, "AV1 Main 8-bit 4:2:0");
            if fits {
                assert_eq!(d.unsupported, None, "{w}x{h}");
            } else {
                let why = d.unsupported.unwrap_or_default();
                assert!(why.contains(&format!("{w}x{h}")) && why.contains("unsupported in this build") && why.contains(reason), "{why}");
            }
        }
        // a record with no sequence header (or a damaged one) says nothing about size: the stream decides
        for obus in [&[][..], &[0x0a, 0x0b][..], &[0xff; 20][..]] {
            assert_eq!(describe(&Av1Config { config_obus: obus.to_vec(), ..base.clone() }).unsupported, None);
        }
    }

    /// The size that is checked when the file opens is the sequence header's declared maximum, not the
    /// size of a coded frame: a record that declares an 8K ceiling is refused up front (before any
    /// frame is read), and one that declares a ceiling that fits is not, whatever its frames hold.
    #[test]
    fn the_up_front_check_reads_the_declared_maximum_size() {
        let declared_8k = describe(&record(8192, 4320, Chroma::C420, false, false));
        assert!(declared_8k.unsupported.is_some_and(|w| w.contains("8192x4320")), "a stream that declares an 8K ceiling is refused when it opens");
        // the same stream declaring the smaller ceiling it actually uses is admitted
        assert_eq!(describe(&record(7680, 4320, Chroma::C420, false, false)).unsupported, None);
        assert_eq!(describe(&record(1920, 1080, Chroma::C420, false, false)).unsupported, None);
        // the decoder itself, given a small frame under a large declared ceiling, is not the one refusing
        // it: its check is per coded frame (`tests/av1_dims.rs` covers the refusals it makes)
        let seq = crate::av1::find_sequence_header(&record(8192, 4320, Chroma::C420, false, false).config_obus).unwrap();
        assert!(crate::av1::picture_fits_budget(&seq, 1920, 1080, crate::av1::DEFAULT_MEMORY_BUDGET));
        assert!(!crate::av1::picture_fits_budget(&seq, seq.max_frame_width, seq.max_frame_height, crate::av1::DEFAULT_MEMORY_BUDGET));
    }

    /// 7680 x 4320 at 4:2:0 is admitted at 8 and 10 bits; at 4:2:2, at 4:4:4 and with film grain it is not.
    #[test]
    fn full_8k_uhd_is_admitted_in_4_2_0_but_not_in_4_4_4_or_with_film_grain() {
        for high in [false, true] {
            let depth = if high { 10 } else { 8 };
            let d = describe(&record(7680, 4320, Chroma::C420, high, false));
            assert_eq!((d.detail.as_str(), d.unsupported), (format!("AV1 Main {depth}-bit 4:2:0").as_str(), None), "8K UHD 4:2:0 at {depth} bits");
            for (chroma, grain, name, detail) in [
                (Chroma::C444, false, "4:4:4", "AV1 High 8-bit 4:4:4"),
                (Chroma::C420, true, "4:2:0 with film grain", "AV1 Main 8-bit 4:2:0"),
                (Chroma::C444, true, "4:4:4 with film grain", "AV1 High 8-bit 4:4:4"),
                (Chroma::C422, false, "4:2:2", "AV1 Professional 8-bit 4:2:2"),
            ] {
                let d = describe(&record(7680, 4320, chroma, high, grain));
                let want_detail = if high { detail.replace("8-bit", "10-bit") } else { detail.to_string() };
                assert_eq!(d.detail, want_detail);
                let why = d.unsupported.unwrap_or_default();
                assert!(
                    why.contains("AV1 7680x4320 pictures are unsupported in this build") && why.contains("picture too large for the memory budget"),
                    "8K UHD {name} at {depth} bits must be refused up front, got {why:?}"
                );
            }
        }
        // film grain and 4:4:4 are fine at sizes their cost allows
        assert_eq!(describe(&record(1920, 1080, Chroma::C444, true, true)).unsupported, None);
        assert_eq!(describe(&record(5232, 2943, Chroma::C444, false, true)).unsupported, None);
        assert!(describe(&record(5248, 2952, Chroma::C444, false, true)).unsupported.is_some());
    }

    /// An MP4 with one AV1 sample whose `av1C` carries `cfg`.
    fn av1_movie(w: u16, h: u16, cfg: Av1Config) -> Result<crate::Movie> {
        use crate::isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
        let container = |e: crate::isobmff::Error| VideoError::Container(e.to_string());
        let mut mw = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).map_err(container)?;
        let entry = SampleEntry::video(FourCc(*b"av01"), crate::isobmff::CodecConfig::Av1(cfg), w, h);
        let t = mw.add_track(TrackConfig::new(entry, 24)).map_err(container)?;
        mw.write_sample(t, WriteSample { data: &[0u8; 16], duration: 1, composition_offset: 0, is_sync: true }).map_err(container)?;
        crate::Movie::open(mw.finish().map_err(container)?.into_inner())
    }

    #[test]
    fn an_8k_uhd_av1_movie_opens_as_decodable_and_the_refused_ones_say_why_on_every_request() {
        // 4:2:0 at 8 and 10 bits: decodable (no frame is requested; the sample is not a real picture)
        for high in [false, true] {
            let m = av1_movie(7680, 4320, record(7680, 4320, Chroma::C420, high, false)).unwrap();
            let i = m.info();
            assert_eq!((i.codec.as_str(), i.width, i.height, i.decodable), ("av1", 7680, 4320, true));
        }
        // 4:4:4, film grain and DCI 8K: not decodable, with the reason, on every request
        for (w, h, chroma, grain) in [(7680, 4320, Chroma::C444, false), (7680, 4320, Chroma::C420, true), (8192, 4320, Chroma::C420, false)] {
            let mut m = av1_movie(w as u16, h as u16, record(w, h, chroma, false, grain)).unwrap();
            let i = m.info().clone();
            assert_eq!((i.codec.as_str(), i.width, i.height, i.decodable), ("av1", w, h, false));
            for _ in 0..2 {
                let Err(VideoError::Unsupported(why)) = m.frame(0) else { panic!("an {w}x{h} AV1 picture must be refused") };
                assert!(why.contains("too large for the memory budget"), "{why}");
            }
            assert!(matches!(m.thumbnail_at(0.0, 16), Err(VideoError::Unsupported(_))));
        }
    }

    #[test]
    fn errors_map_to_stop_or_skip() {
        assert!(matches!(map_error(crate::av1::Error::Unsupported("seq_profile > 2")), VideoError::Unsupported(w) if w.contains("seq_profile")));
        assert!(matches!(map_error(crate::av1::Error::Invalid("x")), VideoError::Decode(_)));
        assert!(matches!(map_error(crate::av1::Error::Truncated), VideoError::Decode(_)));
    }

    #[test]
    fn a_decoder_opens_with_any_config_record_and_decodes_from_the_stream() {
        let frames = ivf_frames(IVF);
        assert_eq!(frames.len(), 4);
        // The sequence header OBUs of the first temporal unit, as an `av1C` would carry them.
        for config in [&[][..], &[0x0a, 0x0b][..], &[0xff; 40][..], &frames[0][..4.min(frames[0].len())]] {
            let mut dec = open(config).expect("opening never fails on a damaged record");
            let mut pics = Vec::new();
            for (i, f) in frames.iter().enumerate() {
                pics.extend(dec.decode(f, 10 + i as i64).expect("valid frames decode"));
            }
            pics.extend(dec.flush());
            assert_eq!(pics.len(), 4);
            assert_eq!(pics.iter().map(|p| p.pts).collect::<Vec<_>>(), [10, 11, 12, 13]);
            let y = &pics[0].yuv;
            assert_eq!((y.width, y.height, y.bit_depth, y.cx, y.cy, y.y_stride, y.uv_stride), (64, 64, 8, 2, 2, 64, 32));
            assert!(matches!(&y.planes, Planes::U8 { y, u, v } if y.len() == 4096 && u.len() == 1024 && v.len() == 1024));
        }
    }

    #[test]
    fn random_access_points_are_found_by_looking_at_the_frame_header() {
        let frames = ivf_frames(IVF);
        let dec = open(&[]).unwrap();
        assert!(dec.checks_random_access());
        assert!(dec.is_random_access(&frames[0]), "the first temporal unit starts with a shown key frame");
        for f in &frames[1..] {
            assert!(!dec.is_random_access(f), "the later units are inter frames or show_existing_frame");
        }
        for junk in [&[][..], &[0xff][..], &[0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..], &[0x32, 0x00][..]] {
            assert!(!dec.is_random_access(junk));
        }
    }

    fn picture(width: u32, height: u32, depth: u8, ss: (u8, u8), mono: bool, matrix: u8, fill: [u16; 3]) -> Picture {
        let cw = (width + u32::from(ss.0)) >> ss.0;
        let ch = (height + u32::from(ss.1)) >> ss.1;
        let planes = [
            vec![fill[0]; (width * height) as usize],
            if mono { vec![] } else { vec![fill[1]; (cw * ch) as usize] },
            if mono { vec![] } else { vec![fill[2]; (cw * ch) as usize] },
        ];
        Picture {
            width,
            height,
            bit_depth: depth,
            subsampling_x: ss.0,
            subsampling_y: ss.1,
            mono_chrome: mono,
            planes,
            color_primaries: 1,
            transfer_characteristics: 1,
            matrix_coefficients: matrix,
            full_range: false,
            pts: 5,
            draft: false,
        }
    }

    #[test]
    fn pictures_of_every_layout_become_planar_pictures() {
        // 4:2:0 10-bit keeps 16-bit samples; 4:2:2 and 4:4:4 set the chroma steps; monochrome has no chroma.
        let p = pic(picture(6, 4, 10, (1, 1), false, 1, [400, 512, 512])).unwrap();
        assert!(matches!(&p.yuv.planes, Planes::U16 { u, .. } if u.len() == 6));
        assert_eq!((p.yuv.cx, p.yuv.cy, p.yuv.uv_stride, p.pts), (2, 2, 3, 5));
        let p = pic(picture(5, 3, 12, (1, 0), false, 1, [400, 2048, 2048])).unwrap();
        assert_eq!((p.yuv.cx, p.yuv.cy, p.yuv.uv_stride), (2, 1, 3));
        assert_eq!(p.yuv.to_rgba().len(), 5 * 3 * 4);
        let p = pic(picture(4, 4, 8, (0, 0), false, 1, [100, 128, 128])).unwrap();
        assert!(matches!(&p.yuv.planes, Planes::U8 { .. }));
        assert_eq!((p.yuv.cx, p.yuv.cy), (1, 1));
        let p = pic(picture(4, 4, 8, (1, 1), true, 1, [235, 0, 0])).unwrap();
        assert_eq!(&p.yuv.to_rgba()[..4], &[255, 255, 255, 255], "monochrome reads as grey");
    }

    #[test]
    fn inconsistent_pictures_are_refused() {
        let mut p = picture(8, 8, 8, (1, 1), false, 1, [16, 128, 128]);
        p.planes[1].pop();
        assert!(matches!(pic(p), Err(VideoError::Decode(_))));
        let mut p = picture(8, 8, 8, (1, 1), false, 1, [16, 128, 128]);
        p.planes[0].push(0);
        assert!(pic(p).is_err());
        let mut p = picture(8, 8, 8, (1, 1), true, 1, [16, 0, 0]);
        p.planes[1] = vec![0; 4];
        assert!(pic(p).is_err());
        assert!(pic(picture(0, 8, 8, (1, 1), true, 1, [0, 0, 0])).is_err());
        let mut huge = picture(2, 2, 8, (1, 1), true, 1, [0, 0, 0]);
        huge.width = 100_000;
        huge.height = 100_000;
        assert!(pic(huge).is_err());
    }

    #[test]
    fn identity_matrix_pictures_are_rgb() {
        // GBR 4:4:4: Y holds G, U holds B, V holds R. Full-range and limited-range, 8 and 10 bits.
        for (depth, full) in [(8u8, true), (8, false), (10, true), (10, false)] {
            let s = depth - 8;
            let (lo, hi) = if full { (0u16, (1u16 << depth) - 1) } else { (16 << s, 235 << s) };
            let mid = (lo + hi) / 2;
            // (G, B, R) -> expected (R, G, B) 8-bit
            for (gbr, want) in [
                ([hi, lo, lo], [0u8, 255, 0]),
                ([lo, hi, lo], [0, 0, 255]),
                ([lo, lo, hi], [255, 0, 0]),
                ([hi, hi, hi], [255, 255, 255]),
                ([lo, lo, lo], [0, 0, 0]),
                ([mid, mid, mid], [128, 128, 128]),
            ] {
                let mut p = picture(2, 2, depth, (0, 0), false, 0, gbr);
                p.full_range = full;
                let rgba = pic(p).unwrap().yuv.to_rgba();
                for c in 0..3 {
                    assert!(
                        i32::from(rgba[c]).abs_diff(i32::from(want[c])) <= 3,
                        "{depth}-bit full={full} gbr={gbr:?}: {:?} vs {want:?}",
                        &rgba[..3]
                    );
                }
            }
        }
    }
}
