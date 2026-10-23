//! VC-3 codec tests that need no outside tool: encoder-to-decoder round trips, and hostile input
//! (corrupt, truncated and absurd headers) which must come back as errors, never panics or huge
//! allocations. The oracle comparison against ffmpeg is `oracle.rs` (ignored by default).

use super::decode::decode_bytes;
use super::header::{self, FieldCode, FrameHeader};
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

/// A synthetic picture: gradients, a ring pattern and some hashed texture.
pub(super) fn picture(w: u32, h: u32, chroma: ChromaFormat, depth: u8, seed: u32) -> Frame {
    let mut f = Frame::new(w, h, chroma, depth, false);
    let max = (1u32 << depth) - 1;
    let s = u32::from(depth) - 8;
    let hash = |x: u32, y: u32| (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ seed.wrapping_mul(83492791)) % 17;
    for y in 0..h {
        for x in 0..w {
            let r = (((x as f32 - w as f32 / 2.0).powi(2) + (y as f32 - h as f32 / 2.0).powi(2)).sqrt() / 6.0).sin();
            let v = (16 << s) as f32 + (219 << s) as f32 * (0.5 + 0.3 * r + 0.2 * (x as f32 / w as f32 - 0.5)) + (hash(x, y) << s) as f32;
            f.y[(y * w + x) as usize] = (v as u32).min(max) as u16;
        }
    }
    let cw = f.chroma_width();
    for y in 0..f.chroma_height() {
        for x in 0..cw {
            let i = (y * cw + x) as usize;
            f.cb[i] = (((128 + (x * 90 / cw) as i32 - 45) as u32) << s) as u16;
            f.cr[i] = (((128 + (y * 80 / f.chroma_height()) as i32 - 40) as u32) << s) as u16;
        }
    }
    f
}

fn psnr(a: &[u16], b: &[u16], peak: f64) -> f64 {
    let mse: f64 = a.iter().zip(b).map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (peak * peak / mse).log10() }
}

fn single_thread() -> DecodeOptions {
    DecodeOptions { threads: false, ..Default::default() }
}

/// Encoder for `profile` at `depth` bits.
pub(super) fn encoder(profile: Profile, depth: u8, w: u32, h: u32) -> Encoder {
    let mut cfg = EncoderConfig::new(profile, w, h);
    cfg.bit_depth = depth;
    Encoder::with_config(cfg).expect("encoder")
}

#[test]
fn roundtrip_all_profiles() {
    // Sizes that are not multiples of 16 (the picture is cropped from whole macroblocks).
    for (profile, depth, w, h, min_db) in [
        (Profile::Lb, 8, 320, 180, 30.0),
        (Profile::Sq, 8, 256, 144, 36.0),
        (Profile::Hq, 8, 178, 122, 45.0),
        (Profile::Hqx, 10, 250, 142, 40.0),
        (Profile::Hqx, 12, 128, 72, 45.0),
        (Profile::R444, 10, 100, 75, 45.0),
        (Profile::R444, 12, 64, 48, 45.0),
    ] {
        let mut enc = encoder(profile, depth, w, h);
        let chroma = enc.config().chroma();
        for seed in 0..2 {
            let src = picture(w, h, chroma, depth, seed);
            let data = enc.encode(&src).unwrap();
            assert_eq!(data.len() as u32, enc.frame_size());
            assert_eq!(&data[data.len() - 4..], &[0x60, 0x0D, 0xC0, 0xDE]);
            let hdr = probe(&data).unwrap();
            assert_eq!((hdr.cid, hdr.width, hdr.lines, hdr.bit_depth), (profile.cid(), w, h, depth));
            let dec = decode_frame(&data).unwrap();
            assert_eq!(dec, decode_frame_with(&data, &single_thread()).unwrap(), "threaded and single-threaded decodes differ");
            assert_eq!((dec.width, dec.height, dec.chroma, dec.bit_depth, dec.rgb), (w, h, chroma, depth, false));
            let peak = f64::from((1u32 << depth) - 1);
            let (p, pc) = (psnr(&src.y, &dec.y, peak), psnr(&src.cb, &dec.cb, peak));
            println!("{profile:?} {depth}-bit {w}x{h} seed {seed}: PSNR {p:.1}/{pc:.1} dB");
            assert!(p > min_db && pc > min_db, "{profile:?} {depth}-bit {w}x{h}: PSNR {p:.1}/{pc:.1}");
        }
    }
}

#[test]
fn encoder_rejects_bad_input() {
    let mut enc = Encoder::new(Profile::Hq, 64, 64).unwrap();
    assert!(enc.encode(&Frame::new(32, 64, ChromaFormat::Yuv422, 8, false)).is_err());
    assert!(enc.encode(&Frame::new(64, 64, ChromaFormat::Yuv444, 8, false)).is_err());
    let mut short = Frame::new(64, 64, ChromaFormat::Yuv422, 8, false);
    short.cb.truncate(10);
    assert!(enc.encode(&short).is_err());
    assert!(Encoder::with_config(EncoderConfig { bit_depth: 10, ..EncoderConfig::new(Profile::Hq, 64, 64) }).is_err());
    assert!(Encoder::new(Profile::Hq, 0, 0).is_err());
    assert!(Encoder::new(Profile::Hq, 20_000, 64).is_err());
    assert!(Encoder::new(Profile::Hq, 8192, 8192 + 16).is_err());
    // A frame is never built bigger than the decoder accepts, whatever the caller asks.
    assert!(Frame::try_new(u32::MAX, u32::MAX, ChromaFormat::Yuv422, 8, false).is_err());
    assert!(Frame::try_new(64, 64, ChromaFormat::Yuv422, 99, false).is_err());
    let none = Frame::new(u32::MAX, u32::MAX, ChromaFormat::Yuv444, 8, true);
    assert_eq!((none.width, none.height, none.y.len()), (0, 0, 0));
}

#[test]
fn short_and_zero_data_is_an_error() {
    for len in [0usize, 1, 4, 100, 0x27f, 0x280, 1000, 0x2000] {
        let zeros = vec![0u8; len];
        assert!(decode_frame(&zeros).is_err() && probe(&zeros).is_err(), "{len} zero bytes");
        let ones = vec![0xffu8; len];
        assert!(decode_frame(&ones).is_err() && probe(&ones).is_err(), "{len} 0xff bytes");
    }
}

// ---------------------------------------------------------------------------------------------
// Hostile input

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Decode `d` every way there is, failing the test on a panic. Errors are fine.
pub(super) fn survives(d: &[u8], label: &str) {
    let res = catch_unwind(AssertUnwindSafe(|| {
        let _ = probe(d);
        let _ = header::parse_prefix(d);
        for opts in [DecodeOptions::default(), DecodeOptions { threads: true, spec_dequant: true }] {
            let _ = decode_frame_with(d, &opts);
        }
    }));
    assert!(res.is_ok(), "panic on {label}");
}

/// Small encoder-made frames, one per kind of stream.
fn seeds() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    for (profile, depth, w, h) in [
        (Profile::Lb, 8, 64u32, 48u32),
        (Profile::Hq, 8, 48, 32),
        (Profile::Hqx, 10, 64, 32),
        (Profile::Hqx, 12, 48, 32),
        (Profile::R444, 10, 48, 32),
    ] {
        let mut enc = encoder(profile, depth, w, h);
        let mut f = Frame::new(w, h, enc.config().chroma(), depth, false);
        for (i, s) in f.y.iter_mut().enumerate() {
            *s = ((16 + (i * 37) % 200) as u16) << (depth - 8);
        }
        if f.chroma != ChromaFormat::Yuv444 {
            f.cb.fill(128 << (depth - 8));
            f.cr.fill(100 << (depth - 8));
        }
        v.push(enc.encode(&f).unwrap());
    }
    v
}

fn fuzz_iterations() -> usize {
    std::env::var("PA_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(200)
}

#[test]
fn corrupt_streams_never_panic() {
    let mut enc = Encoder::new(Profile::Hq, 256, 128).unwrap();
    let good = enc.encode(&picture(256, 128, ChromaFormat::Yuv422, 8, 1)).unwrap();
    let mut rng = Rng(0x1234_5678);
    for i in 0..fuzz_iterations() {
        let mut d = good.clone();
        for _ in 0..1 + rng.below(8) {
            // mostly the payload, sometimes the header
            let pos = if i % 4 == 0 { rng.below(0x280) } else { 0x280 + rng.below(d.len() - 0x280) };
            d[pos] ^= 1 << rng.below(8);
        }
        if i % 5 == 0 {
            d.truncate(rng.below(d.len()));
        }
        survives(&d, &format!("mutation {i}"));
    }
}

#[test]
fn every_header_byte_and_every_truncation_is_survived() {
    for (si, seed) in seeds().iter().enumerate() {
        let stride = (seed.len() / 100).max(1);
        for n in (0..seed.len()).step_by(stride) {
            survives(&seed[..n], &format!("seed {si} truncated to {n}"));
        }
        // The header and the start of the scan-line index: every byte to extreme values, every bit.
        for p in 0..seed.len().min(0x300) {
            for v in [0u8, 0xff, 0x80, 0x01, 0x7f] {
                let mut d = seed.clone();
                d[p] = v;
                survives(&d, &format!("seed {si} byte {p:#x}={v:#x}"));
            }
            for b in 0..8 {
                let mut d = seed.clone();
                d[p] ^= 1 << b;
                survives(&d, &format!("seed {si} flip {p:#x}.{b}"));
            }
        }
        // Big dimension fields with the rest intact (lines 0x18, width 0x1A, macroblock rows 0x16C).
        for (off, v) in [(0x18usize, 0xffffu16), (0x1a, 0xffff), (0x18, 0x4000), (0x1a, 0x4000), (0x16c, 0xffff)] {
            let mut d = seed.clone();
            d[off..off + 2].copy_from_slice(&v.to_be_bytes());
            survives(&d, &format!("seed {si} u16 {off:#x}={v:#x}"));
        }
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ si as u64);
        for i in 0..fuzz_iterations() {
            let mut d = seed.clone();
            for _ in 0..1 + rng.below(8) {
                let p = rng.below(d.len());
                if rng.below(2) == 0 {
                    d[p] ^= 1 << rng.below(8);
                } else {
                    d[p] = rng.next() as u8;
                }
            }
            survives(&d, &format!("seed {si} random {i}"));
        }
    }
}

mod fuzz {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::with_cases(48) })]

        /// Any pile of byte changes to a good frame decodes or fails, and never panics.
        #[test]
        fn mutated_frames_never_panic(edits in proptest::collection::vec((any::<u16>(), any::<u8>()), 1..24), cut in 0u8..4) {
            let mut enc = Encoder::new(Profile::Sq, 80, 48).unwrap();
            let mut d = enc.encode(&picture(80, 48, ChromaFormat::Yuv422, 8, 3)).unwrap();
            for (at, v) in edits {
                let n = d.len();
                d[usize::from(at) % n] = v;
            }
            if cut == 0 {
                d.truncate(d.len() / 2);
            }
            let survived = catch_unwind(AssertUnwindSafe(|| decode_frame_with(&d, &single_thread()).is_ok())).is_ok();
            prop_assert!(survived);
        }

        /// Arbitrary bytes behind a plausible header (its fields all in range) decode or fail.
        #[test]
        fn random_payloads_never_panic(payload in proptest::collection::vec(any::<u8>(), 0..4096), cid in 0usize..20, chroma in 0u8..3) {
            let info = &tables::CIDS[cid];
            let chroma = [ChromaFormat::Yuv420, ChromaFormat::Yuv422, ChromaFormat::Yuv444][usize::from(chroma)];
            let h = header_for(info.cid, chroma, 8, 32, false, true);
            let d = craft(&h, &payload);
            let survived = catch_unwind(AssertUnwindSafe(|| decode_frame_with(&d, &single_thread()).is_ok())).is_ok();
            prop_assert!(survived);
        }
    }
}

/// A header for compression ID `cid` with the raster that ID needs (a fixed width for the HD
/// profile), `lines` lines per coding unit and everything else plain.
fn header_for(cid: u32, chroma: ChromaFormat, depth: u8, lines: u32, interlaced: bool, frame_encoding: bool) -> FrameHeader {
    let width = match tables::cid_info(cid).map(|i| i.kind) {
        Some(tables::Kind::Hd { width, .. }) => u32::from(width),
        _ => 48,
    };
    FrameHeader {
        header_size: header::header_size_for(lines),
        version: 3,
        cid,
        vbr: true,
        field: FieldCode::Frame,
        macf: false,
        crc: false,
        alpha: false,
        lossless_alpha: false,
        premultiplied_alpha: false,
        width,
        lines,
        par: (0, 0),
        bit_depth: depth,
        interlaced,
        frame_encoding,
        chroma,
        rgb: false,
        color_volume: ColorVolume::Bt709,
        timecode: None,
        mb_rows: lines.div_ceil(16),
    }
}

/// A coding unit with header `h` and `payload` behind it, shared out evenly over the scan lines.
fn craft(h: &FrameHeader, payload: &[u8]) -> Vec<u8> {
    let mut d = header::write(h);
    let rows = h.mb_rows.max(1) as usize;
    let per = payload.len() / rows;
    for s in 0..h.mb_rows as usize {
        let o = header::SCAN_INDEX_OFFSET + 4 * s;
        if let Some(slot) = d.get_mut(o..o + 4) {
            slot.copy_from_slice(&((s * per) as u32).to_be_bytes());
        }
    }
    d.extend_from_slice(payload);
    d
}

/// Every compression ID with every chroma format, depth, scan and alpha mode in the header over a
/// random payload: the decoder takes what it can and refuses the rest, and never panics.
#[test]
fn every_header_combination_is_survived() {
    let mut rng = Rng(0xdead_beef_1234_5678);
    let (mut parsed, mut decoded) = (0usize, 0usize);
    for info in tables::CIDS.iter() {
        for chroma in [ChromaFormat::Yuv420, ChromaFormat::Yuv422, ChromaFormat::Yuv444] {
            for depth in [8u8, 10, 12] {
                for (interlaced, frame_encoding) in [(false, true), (true, true), (true, false)] {
                    for (alpha, lossless_alpha) in [(false, false), (true, false), (true, true)] {
                        for (rgb, macf) in [(false, false), (true, true)] {
                            let mut h = header_for(info.cid, chroma, depth, 32, interlaced, frame_encoding);
                            (h.alpha, h.lossless_alpha, h.rgb, h.macf) = (alpha, lossless_alpha, rgb, macf);
                            if !frame_encoding {
                                h.field = FieldCode::Field1;
                            }
                            let nw = h.mb_width() as usize;
                            let payload: Vec<u8> = (0..h.mb_rows as usize * (nw * 48 + 64)).map(|_| rng.next() as u8).collect();
                            let mut d = craft(&h, &payload);
                            if !frame_encoding {
                                // field coded: a second coding unit follows
                                let mut second = craft(&FrameHeader { field: FieldCode::Field2, ..h.clone() }, &payload);
                                d.append(&mut second);
                            }
                            let label = format!(
                                "cid {} {chroma:?} {depth}-bit interlaced {interlaced} frame_encoding {frame_encoding} alpha {alpha}/{lossless_alpha} rgb {rgb}",
                                info.cid
                            );
                            survives(&d, &label);
                            parsed += usize::from(header::parse(&d).is_ok());
                            decoded += usize::from(decode_frame_with(&d, &single_thread()).is_ok());
                        }
                    }
                }
            }
        }
    }
    // The sweep reaches the pixel code, not just the header checks.
    println!("{parsed} header combinations parsed, {decoded} decoded");
    assert!(parsed > 300 && decoded > 0, "{parsed} parsed, {decoded} decoded");
}

/// The HD profile is 4:2:2 (4:4:4 only for one ID): its 4:2:0 header used to pass the header
/// checks and then store macroblock-adaptive field rows outside the shorter chroma band.
#[test]
fn hd_profile_without_a_macroblock_layout_is_refused() {
    let mut h = header_for(1260, ChromaFormat::Yuv420, 8, 32, true, true);
    h.macf = true;
    let d = craft(&h, &vec![0x55u8; 4096]);
    assert!(matches!(header::parse(&d), Err(Error::Invalid(_))));
    assert!(decode_frame(&d).is_err());
    for info in tables::CIDS.iter().filter(|i| matches!(i.kind, tables::Kind::Hd { .. })) {
        let h = header_for(info.cid, ChromaFormat::Yuv420, if matches!(info.cid, 1235 | 1241 | 1250) { 10 } else { 8 }, 32, false, true);
        assert!(header::parse(&craft(&h, &[0u8; 64])).is_err(), "cid {}", info.cid);
    }
}

fn assert_refused_fast(d: &[u8], what: &str) {
    let t = Instant::now();
    let r = catch_unwind(AssertUnwindSafe(|| decode_frame_with(d, &single_thread()))).unwrap_or_else(|_| panic!("panic on {what}"));
    assert!(r.is_err(), "{what} decoded");
    assert!(t.elapsed() < Duration::from_secs(5), "{what} took {:?}", t.elapsed());
}

/// Headers claiming a picture the app could never show are refused before any plane is allocated
/// (a 16384 x 16384 header used to ask for about 1.6 GB of planes).
#[test]
fn absurd_headers_are_refused_before_allocating() {
    let tiny = [0x11u8; 256];
    for (w, lines) in [(16384u32, 16384u32), (65535, 65535), (16384, 64), (64, 16384), (8193, 16), (16, 8193)] {
        let mut h = header_for(1272, ChromaFormat::Yuv422, 8, 16, false, true);
        (h.width, h.lines, h.mb_rows) = (w, lines, lines.div_ceil(16));
        // `header_size` stays minimal so the raster is what the header check trips on.
        let d = craft(&FrameHeader { header_size: 0x280, ..h }, &tiny);
        assert!(header::parse(&d).is_err(), "{w}x{lines} parsed");
        assert_refused_fast(&d, &format!("{w}x{lines} header"));
    }
    // The limit on the area counts both fields of a field-coded frame.
    let mut h = header_for(1272, ChromaFormat::Yuv422, 8, 8192, true, false);
    (h.width, h.lines, h.mb_rows) = (8192, 8192, 512);
    assert!(header::parse(&craft(&h, &tiny)).is_err());
    // A raster at the limit with no data to fill it: refused as truncated, quickly, without the
    // planes (they would be 0.5 GB).
    let h = header_for(1272, ChromaFormat::Yuv422, 8, 8192, false, true);
    let h = FrameHeader { width: 8192, ..h };
    let d = craft(&h, &tiny);
    assert!(header::parse(&d).is_ok());
    assert_refused_fast(&d, "8192x8192 header over 256 bytes");
    // The largest accepted picture (4:4:4 with alpha, both of whose planes sets are counted) is
    // exactly the decode memory budget; the budget only bites if the picture limit is ever raised.
    let mut h = header_for(1270, ChromaFormat::Yuv444, 10, 8192, false, true);
    (h.width, h.alpha) = (8192, true);
    assert_eq!(decode_bytes(&h), decode::MAX_DECODE_BYTES);
    assert_refused_fast(&craft(&h, &tiny), "8192x8192 4:4:4 with alpha over 256 bytes");
    assert!(decode_bytes(&FrameHeader { width: 9000, ..h }) > decode::MAX_DECODE_BYTES);
}

#[test]
fn damaged_scan_line_indices_are_refused() {
    for (si, seed) in seeds().iter().enumerate() {
        let h = probe(seed).unwrap();
        let o = header::SCAN_INDEX_OFFSET;
        for value in [u32::MAX, u32::MAX / 2, seed.len() as u32, 0x8000_0000, 1] {
            let mut d = seed.clone();
            for s in 0..h.mb_rows as usize {
                d[o + 4 * s..o + 4 * s + 4].copy_from_slice(&value.to_be_bytes());
            }
            survives(&d, &format!("seed {si} scan line index {value:#x}"));
        }
        // all rows share one start: still bounded by the macroblock count
        let mut d = seed.clone();
        d[o..o + 4 * h.mb_rows as usize].fill(0);
        survives(&d, &format!("seed {si} zero indices"));
    }
}

/// A header longer than 640 bytes (a raster above 1088 lines) can be read from the first 640 bytes
/// of a sample; only a full parse needs the scan-line indices.
#[test]
fn prefix_parse_reads_a_long_header_from_its_first_640_bytes() {
    let h = header_for(1272, ChromaFormat::Yuv422, 8, 1200, false, true);
    assert!(h.header_size > 0x280);
    let d = craft(&h, &vec![0u8; 8192]);
    let p = header::parse_prefix(&d[..0x280]).unwrap();
    assert_eq!((p.cid, p.lines, p.header_size), (1272, 1200, h.header_size));
    assert_eq!(header::parse(&d[..0x280]), Err(Error::Truncated));
    assert!(header::parse(&d).is_ok());
    assert_eq!(header::parse_prefix(&d[..0x27f]), Err(Error::Truncated));
}

#[test]
fn compression_id_names_are_neutral() {
    let forbidden = ["apple", "prores", "avid", "dnx"];
    for info in tables::CIDS.iter() {
        for depth in [8u8, 10, 12] {
            for text in [cid_name(info.cid), cid_variant(info.cid), label(info.cid, depth)] {
                let lower = text.to_ascii_lowercase();
                assert!(!forbidden.iter().any(|w| lower.contains(w)), "{text}");
            }
        }
    }
    assert_eq!(label(1271, 10), "VC-3 (HQX, 10-bit)");
    assert_eq!(label(1273, 8), "VC-3 (SQ, 8-bit)");
    assert_eq!(label(1242, 8), "VC-3 (1080i CID 1242, 8-bit)");
    assert_eq!(cid_name(1274), "VC-3 LB");
    assert_eq!(label(4242, 8), "VC-3 (CID 4242, 8-bit)");
    for p in [Profile::Lb, Profile::Sq, Profile::Hq, Profile::Hqx, Profile::R444] {
        let lower = p.name().to_ascii_lowercase();
        assert!(lower.starts_with("vc-3 ") && !forbidden.iter().any(|w| lower.contains(w)));
    }
    // Error messages name the standard only.
    for e in [Error::Truncated, Error::Invalid("raster size"), Error::UnknownCid(7), Error::Row(3), Error::Input("x".into())] {
        let lower = e.to_string().to_ascii_lowercase();
        assert!(!forbidden.iter().any(|w| lower.contains(w)), "{e}");
    }
}
