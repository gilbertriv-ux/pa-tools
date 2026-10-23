//! HEVC (H.265) through the public API, in both feature states.
//!
//! The samples in `fixtures/hevc/` are 64x64 synthetic test patterns (see `ATTRIBUTION.md`). The
//! reference hashes are the SHA-256 of ffmpeg's raw decode of each sample (frames in output order,
//! planar 4:2:0, 8-bit or 16-bit little endian), recorded when the samples were made. Nothing here
//! runs an external tool.
//!
//! Without the `hevc-decode` feature a movie of HEVC opens, says what it is, and says its pictures
//! are unsupported in this build. With it, the pictures decode bit-exactly.

use soundcraft_video::isobmff::{Brand, HevcConfig, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use soundcraft_video::{Movie, VideoError};
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};

const MAIN8: &[u8] = include_bytes!("fixtures/hevc/hevc-main-8bit-64x64-4f.mp4");
const MAIN10: &[u8] = include_bytes!("fixtures/hevc/hevc-main10-64x64-4f.mp4");
const BFRAMES: &[u8] = include_bytes!("fixtures/hevc/hevc-main-8bit-64x64-bframes2-ref2-48f.mp4");
/// An open GOP: an IDR picture, then clean random access (CRA) pictures every 8 frames, the third and
/// fourth of which have leading pictures (RASL) that predict from before the CRA.
const OPEN_GOP: &[u8] = include_bytes!("fixtures/hevc/hevc-main-8bit-64x64-opengop-32f.mp4");

/// (name, movie, frames, profile label, SHA-256 of the reference decode).
const MOVIES: [(&str, &[u8], usize, &str, &str); 4] = [
    ("main8", MAIN8, 4, "H.265 Main", "fe81f18a9188107c96b53ee241b477689fdff010dcbd52cec5a1033d5ce3aa40"),
    ("main10", MAIN10, 4, "H.265 Main 10", "3fe0777f821dd7c0ef982a00660a54b80dd24eefb8da4be121c61e2f86876362"),
    ("bframes", BFRAMES, 48, "H.265 Main", "9a995aa1c9902ac309b945ec3126447f2212e97bc2285f5a84b49c6e62030298"),
    ("opengop", OPEN_GOP, 32, "H.265 Main", "0e6b618e54886fb02efb3ea389cef9f9e5da87308be5c657094cb1efbe6668cf"),
];

const ENABLED: bool = cfg!(feature = "hevc-decode");

fn open(bytes: &[u8]) -> Movie {
    Movie::open(bytes.to_vec()).expect("a movie")
}

#[test]
fn the_samples_open_as_hevc_in_either_build() {
    for (name, bytes, frames, label, _) in MOVIES {
        let m = open(bytes);
        let i = m.info();
        assert_eq!((i.codec.as_str(), i.width, i.height, i.frame_count), ("hevc", 64, 64, frames), "{name}");
        assert_eq!(i.codec_detail, label, "{name}");
        assert_eq!(i.decodable, ENABLED, "{name}");
    }
}

#[test]
fn a_build_without_the_decoder_says_so() {
    if ENABLED {
        return;
    }
    for (name, bytes, ..) in MOVIES {
        let mut m = open(bytes);
        let Err(VideoError::Unsupported(why)) = m.frame(0) else { panic!("{name}: should be unsupported") };
        assert!(why.contains("unsupported in this build"), "{why}");
        assert!(matches!(m.thumbnail_at(0.0, 16), Err(VideoError::Unsupported(_))));
    }
}

/// An in-memory movie of one HEVC sample entry, to see what the movie says about it.
fn one_sample_movie(cfg: HevcConfig) -> Movie {
    let mut mw = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).expect("writer");
    let t = mw.add_track(TrackConfig::new(SampleEntry::hevc(cfg, 64, 48), 24)).expect("track");
    mw.write_sample(t, WriteSample { data: &[0u8; 16], duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
    Movie::open(mw.finish().expect("finish").into_inner()).expect("movie")
}

#[test]
fn variants_the_decoder_cannot_do_open_with_the_reason() {
    let base =
        HevcConfig { general_profile_idc: 4, chroma_format_idc: 1, bit_depth_luma: 8, bit_depth_chroma: 8, length_size: 4, ..Default::default() };
    let cases = [
        (HevcConfig { chroma_format_idc: 2, ..base.clone() }, "4:2:2"),
        (HevcConfig { chroma_format_idc: 3, ..base.clone() }, "4:4:4"),
        (HevcConfig { chroma_format_idc: 0, ..base.clone() }, "monochrome"),
        (HevcConfig { bit_depth_luma: 14, bit_depth_chroma: 14, ..base.clone() }, "14-bit"),
    ];
    for (cfg, what) in cases {
        let mut m = one_sample_movie(cfg);
        assert_eq!(m.info().codec, "hevc");
        assert!(!m.info().decodable, "{what}");
        let Err(VideoError::Unsupported(why)) = m.frame(0) else { panic!("{what}: should be unsupported") };
        // (a build without the decoder gives its own reason first)
        assert!(why.contains(if ENABLED { what } else { "unsupported in this build" }), "{what}: {why}");
        assert!(!m.info().codec_detail.to_ascii_lowercase().contains("apple"));
    }
    // Luma and chroma at different depths (valid in Main 10) pass too; they decode, see the synthetic
    // stream test in `src/hevc/synth_tests.rs`.
    let mixed = one_sample_movie(HevcConfig { general_profile_idc: 2, bit_depth_luma: 8, bit_depth_chroma: 10, ..base.clone() });
    assert_eq!(mixed.info().decodable, ENABLED);
    // Main 10 at 4:2:0 passes the pre-check (the sample itself is not a picture; that is a decode error)
    let ok = one_sample_movie(HevcConfig { general_profile_idc: 2, bit_depth_luma: 10, bit_depth_chroma: 10, ..base });
    assert_eq!(ok.info().decodable, ENABLED);
    assert_eq!(ok.info().codec_detail, "H.265 Main 10");
}

/// Mutated and truncated movies never panic, whatever the build.
#[test]
fn mutated_and_truncated_hevc_movies_never_panic() {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for (name, bytes, ..) in MOVIES {
        let iters = std::env::var("HEVC_FUZZ_ITERS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(60);
        for it in 0..iters {
            let mut b = bytes.to_vec();
            match it % 3 {
                0 => b.truncate((next() as usize) % (bytes.len() + 1)),
                1 => {
                    for _ in 0..1 + next() % 6 {
                        let p = (next() as usize) % b.len();
                        b[p] ^= (next() % 255 + 1) as u8;
                    }
                }
                _ => {
                    // damage the sample data only (the second half of the file holds the pictures)
                    for _ in 0..1 + next() % 10 {
                        let p = b.len() / 2 + (next() as usize) % (b.len() / 2);
                        b[p] = next() as u8;
                    }
                }
            }
            let r = catch_unwind(AssertUnwindSafe(|| {
                if let Ok(mut m) = Movie::open(b) {
                    let n = m.info().frame_count.min(12);
                    for i in 0..n {
                        let _ = m.frame(i);
                    }
                    let _ = m.thumbnail_at(0.0, 16);
                }
            }));
            assert!(r.is_ok(), "{name} iteration {it} panicked");
        }
    }
}

#[cfg(feature = "hevc-decode")]
mod decoding {
    use super::*;
    use soundcraft_video::convert::{Matrix, planar_to_rgba, planar16_to_rgba};
    use soundcraft_video::hevc::{Decoder, Picture, Plane};
    use soundcraft_video::isobmff::{self, CodecConfig};

    const MAIN8_ANNEXB: &[u8] = include_bytes!("fixtures/hevc/hevc-main-8bit-64x64-4f.h265");
    const MAIN8_ANNEXB_SHA: &str = "fe81f18a9188107c96b53ee241b477689fdff010dcbd52cec5a1033d5ce3aa40";

    /// SHA-256 (FIPS 180-4), so the tests can check ffmpeg's reference hashes without a dependency.
    fn sha256(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
            0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
            0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
            0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
            0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
            0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        let (blocks, _) = msg.as_chunks::<64>();
        for block in blocks {
            let mut w = [0u32; 64];
            let (words, _) = block.as_chunks::<4>();
            for (i, c) in words.iter().enumerate() {
                w[i] = u32::from_be_bytes(*c);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let mut v = h;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                let t2 = s0.wrapping_add(maj);
                v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
            }
            for (a, b) in h.iter_mut().zip(v) {
                *a = a.wrapping_add(b);
            }
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }

    #[test]
    fn sha256_known_answers() {
        assert_eq!(sha256(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(sha256(&vec![b'a'; 1_000_000]), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    /// ffmpeg's `rawvideo` layout: planar, 8-bit samples as bytes, deeper ones as 16-bit little endian.
    fn raw(pics: &[Picture]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in pics {
            for plane in [&p.y, &p.u, &p.v] {
                match plane {
                    Plane::U8(v) => out.extend_from_slice(v),
                    Plane::U16(v) => out.extend(v.iter().flat_map(|s| s.to_le_bytes())),
                }
            }
        }
        out
    }

    /// Every picture of a movie file through the decoder alone (hvcC + length-prefixed samples),
    /// in output order.
    fn decode_mp4(bytes: &[u8]) -> Vec<Picture> {
        let src = bytes.to_vec();
        let mp4 = isobmff::open(src.clone()).expect("mp4");
        let t = mp4.tracks.iter().position(|t| t.kind == isobmff::TrackKind::Video).expect("video track");
        let Some(CodecConfig::Hevc(cfg)) = mp4.tracks[t].codec() else { panic!("not HEVC") };
        let mut dec = Decoder::with_threads(1);
        dec.configure_hvcc(&cfg.to_bytes()).expect("hvcC");
        // presentation index of every sample, as the movie passes it
        let n = mp4.tracks[t].samples.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (mp4.tracks[t].samples[i].pts, i));
        let mut rank = vec![0usize; n];
        for (p, &i) in order.iter().enumerate() {
            rank[i] = p;
        }
        let mut pics = Vec::new();
        for (s, &pts) in rank.iter().enumerate() {
            let data = mp4.read_sample(&src, t, s).expect("sample");
            pics.extend(dec.decode(&data, pts as i64).expect("decode"));
        }
        pics.extend(dec.flush());
        assert!(dec.take_error().is_none());
        pics
    }

    #[test]
    fn decoded_pictures_match_the_ffmpeg_reference_bit_exactly() {
        for (name, bytes, frames, _, sha) in MOVIES {
            let pics = decode_mp4(bytes);
            assert_eq!(pics.len(), frames, "{name}");
            // output order is presentation order, and the pts travelled with each picture
            assert!(pics.iter().enumerate().all(|(i, p)| p.pts == i as i64), "{name}: pts order");
            assert_eq!(sha256(&raw(&pics)), sha, "{name} differs from ffmpeg's decode");
        }
    }

    #[test]
    fn annex_b_stream_matches_the_mp4_and_the_reference() {
        let mut dec = Decoder::with_threads(1);
        let mut pics = dec.decode(MAIN8_ANNEXB, 0).expect("decode");
        pics.extend(dec.flush());
        assert_eq!(pics.len(), 4);
        assert_eq!(sha256(&raw(&pics)), MAIN8_ANNEXB_SHA);
    }

    #[test]
    fn the_picture_planes_have_the_stated_geometry() {
        for (name, bytes, _, _, _) in MOVIES {
            let depth = if name == "main10" { 10 } else { 8 };
            for p in decode_mp4(bytes) {
                assert_eq!((p.width, p.height, p.bit_depth), (64, 64, depth), "{name}");
                assert_eq!((p.y.len(), p.u.len(), p.v.len()), (64 * 64, 32 * 32, 32 * 32), "{name}");
                assert_eq!(p.y.as_u8().is_some(), depth == 8, "{name}");
            }
        }
    }

    /// The RGBA a picture converts to, by the movie's own conversion.
    fn rgba(p: &Picture) -> Vec<u8> {
        let m = Matrix::for_code(p.color.matrix, p.height, p.color.full_range);
        let (w, h) = (p.width as usize, p.height as usize);
        match (&p.y, &p.u, &p.v) {
            (Plane::U8(y), Plane::U8(u), Plane::U8(v)) => planar_to_rgba(w, h, y, p.y_stride, u, v, p.uv_stride, 2, 2, &m),
            (Plane::U16(y), Plane::U16(u), Plane::U16(v)) => planar16_to_rgba(w, h, y, p.y_stride, u, v, p.uv_stride, 2, 2, p.bit_depth as u8, &m),
            _ => panic!("mixed planes"),
        }
    }

    #[test]
    fn the_movie_shows_the_decoded_pictures() {
        for (name, bytes, frames, _, _) in MOVIES {
            let pics = decode_mp4(bytes);
            let mut m = open(bytes);
            assert!(m.info().decodable, "{name}");
            for (i, p) in pics.iter().enumerate() {
                let f = m.frame(i).unwrap_or_else(|e| panic!("{name} frame {i}: {e}"));
                assert_eq!((f.width, f.height, f.index), (64, 64, i));
                assert!(f.rgba == rgba(p), "{name}: frame {i} differs from the converted picture");
            }
            assert_eq!(pics.len(), frames);
        }
    }

    #[test]
    fn random_access_gives_the_sequential_pictures() {
        for (name, bytes, frames, _, _) in MOVIES {
            let mut seq = open(bytes);
            let all: Vec<_> = (0..frames).map(|i| seq.frame(i).expect("sequential")).collect();
            // a fresh movie asked for frames in a scattered order, then backwards
            let mut order: Vec<usize> = (0..frames).map(|k| (k * 7 + 3) % frames).collect();
            order.extend((0..frames).rev());
            let mut m = open(bytes);
            m.set_cache_frames(1);
            for i in order {
                let f = m.frame(i).unwrap_or_else(|e| panic!("{name} frame {i}: {e}"));
                assert!(f.rgba == all[i].rgba, "{name}: frame {i} after seeking differs");
            }
        }
    }

    /// The open-GOP sample's SHA-256 of ffmpeg's raw decode (the `opengop` row of `MOVIES`).
    const OPEN_GOP_SHA: &str = "0e6b618e54886fb02efb3ea389cef9f9e5da87308be5c657094cb1efbe6668cf";

    /// (NAL unit types in decode order of every sample, whether the container flags the sample as a
    /// sync sample) for the open-GOP sample.
    fn open_gop_samples() -> Vec<(Vec<u8>, bool)> {
        let src = OPEN_GOP.to_vec();
        let mp4 = isobmff::open(src.clone()).expect("mp4");
        let t = mp4.tracks.iter().position(|t| t.kind == isobmff::TrackKind::Video).expect("video track");
        let Some(CodecConfig::Hevc(cfg)) = mp4.tracks[t].codec() else { panic!("not HEVC") };
        let length_size = usize::from(cfg.length_size);
        (0..mp4.tracks[t].samples.len())
            .map(|s| {
                let data = mp4.read_sample(&src, t, s).expect("sample");
                let (mut types, mut pos) = (Vec::new(), 0);
                while let Some(len) = data.get(pos..pos + length_size) {
                    let n = len.iter().fold(0usize, |a, &b| (a << 8) | usize::from(b));
                    types.push(data.get(pos + length_size).map_or(255, |h| (h >> 1) & 0x3f));
                    pos += length_size + n;
                }
                (types, mp4.tracks[t].samples[s].is_sync)
            })
            .collect()
    }

    /// The fixture must really be an open GOP, or the tests below prove nothing: an IDR picture first,
    /// then CRA pictures (type 21) flagged as sync samples, with RASL pictures (types 8 and 9, which
    /// predict from before their CRA) in decode order after some of them.
    #[test]
    fn the_open_gop_sample_has_cra_access_points_and_leading_pictures() {
        let samples = open_gop_samples();
        assert_eq!(samples.len(), 32);
        let irap: Vec<(usize, u8)> =
            samples.iter().enumerate().filter_map(|(i, (t, _))| t.iter().copied().find(|t| (16..=23).contains(t)).map(|t| (i, t))).collect();
        let types: Vec<u8> = irap.iter().map(|&(_, t)| t).collect();
        assert_eq!(types, [20, 21, 21, 21], "an IDR_N_LP picture, then three CRA pictures: {irap:?}");
        let sync: Vec<usize> = samples.iter().enumerate().filter(|(_, (_, s))| *s).map(|(i, _)| i).collect();
        assert_eq!(sync, irap.iter().map(|&(i, _)| i).collect::<Vec<_>>(), "the container flags exactly the IRAP samples");
        let rasl = samples.iter().filter(|(t, _)| t.iter().any(|&t| t == 8 || t == 9)).count();
        assert!(rasl >= 3, "{rasl} leading pictures that depend on pictures before their CRA");
    }

    /// Frames in presentation order as RGBA. The decoder alone reproduces ffmpeg's raw decode of the
    /// whole stream (checked here), and each frame is a slice of that decode, so this is ffmpeg's
    /// picture for every index.
    fn open_gop_reference() -> Vec<Vec<u8>> {
        let pics = decode_mp4(OPEN_GOP);
        assert_eq!(pics.len(), 32);
        assert_eq!(sha256(&raw(&pics)), OPEN_GOP_SHA, "differs from ffmpeg's decode");
        pics.iter().map(rgba).collect()
    }

    /// Presentation index of every sync sample (the CRA pictures and the IDR).
    fn open_gop_access_points() -> Vec<usize> {
        let mp4 = isobmff::open(OPEN_GOP.to_vec()).expect("mp4");
        let t = mp4.tracks.iter().position(|t| t.kind == isobmff::TrackKind::Video).expect("video track");
        let samples = &mp4.tracks[t].samples;
        let mut order: Vec<usize> = (0..samples.len()).collect();
        order.sort_by_key(|&i| (samples[i].pts, i));
        let mut points: Vec<usize> = order.iter().enumerate().filter(|&(_, &i)| samples[i].is_sync).map(|(rank, _)| rank).collect();
        points.sort_unstable();
        points
    }

    /// Frame requests in any order, into GOPs that start with a CRA picture, give ffmpeg's pictures,
    /// including the leading pictures that come before their CRA in display order and need pictures
    /// from the GOP before it. One frame cache entry, so every request decodes.
    #[test]
    fn shuffled_seeks_into_cra_gops_match_the_reference() {
        let want = open_gop_reference();
        let cras = open_gop_access_points();
        assert_eq!(cras.len(), 4, "{cras:?}");
        let check = |order: &[usize], what: &str| {
            let mut m = open(OPEN_GOP);
            m.set_cache_frames(1);
            for &i in order {
                let f = m.frame(i).unwrap_or_else(|e| panic!("{what}: frame {i}: {e}"));
                assert!(f.rgba == want[i], "{what}: frame {i} differs from the reference (order {order:?})");
            }
        };
        // Around each CRA: the CRA, the pictures displayed just before it (its leading pictures), then
        // pictures after it, then back before it.
        for &c in cras.iter().filter(|&&c| c > 0) {
            check(&[c, c - 1, c - 2, c + 3, c - 1, c, c - 3], &format!("around the CRA at {c}"));
            check(&[31, c - 1, 0, c, c - 2, 31], &format!("into the CRA at {c} from the ends"));
        }
        // Every picture straight after a jump from the other end.
        for i in 0..32 {
            check(&[31 - i, i], &format!("jump to {i}"));
        }
        // Shuffles.
        let mut x = 0x9e3779b97f4a7c15_u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for round in 0..40 {
            let mut order: Vec<usize> = (0..32).collect();
            for k in (1..order.len()).rev() {
                order.swap(k, (next() as usize) % (k + 1));
            }
            check(&order, &format!("shuffle {round}"));
        }
    }

    /// The same with the default frame cache, so pictures a decode produced on the way (the `ready`
    /// ones) are served to later requests.
    #[test]
    fn shuffled_seeks_with_the_default_cache_match_the_reference() {
        let want = open_gop_reference();
        let mut m = open(OPEN_GOP);
        let mut x = 0x2545f4914f6cdd1d_u64;
        for _ in 0..200 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let i = (x as usize) % 32;
            let f = m.frame(i).unwrap_or_else(|e| panic!("frame {i}: {e}"));
            assert!(f.rgba == want[i], "frame {i} differs from the reference");
        }
    }

    #[test]
    fn keyframe_thumbnails_decode() {
        for (name, bytes, ..) in MOVIES {
            let mut m = open(bytes);
            let t = m.thumbnail_at(0.0, 16).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(t.height, 16, "{name}");
            let last = m.info().duration * 0.99;
            assert!(m.thumbnail_at(last, 16).is_ok(), "{name}");
        }
    }

    #[test]
    fn a_lone_slice_without_parameter_sets_is_a_decode_error_not_a_stop() {
        // a movie whose hvcC holds no parameter sets and whose only sample is junk: opens, and the
        // frame request fails with a decode error (damaged data), not "unsupported"
        let cfg =
            HevcConfig { general_profile_idc: 1, chroma_format_idc: 1, bit_depth_luma: 8, bit_depth_chroma: 8, length_size: 4, ..Default::default() };
        let mut m = one_sample_movie(cfg);
        assert!(m.info().decodable);
        assert!(matches!(m.frame(0), Err(VideoError::Decode(_))));
    }
}
