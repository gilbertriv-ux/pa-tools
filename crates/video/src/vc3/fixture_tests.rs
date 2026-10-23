//! The sample made by a different VC-3 encoder (ffmpeg's, run outside the repository; see
//! `tests/fixtures/vc3/` and its ATTRIBUTION.md row): a 256 x 144, two-frame test-pattern movie. It
//! guards the one thing our own encoder cannot, agreement with the stream the rest of the world
//! writes and reads. The reference values are ffmpeg's decode of the same file, sampled on a
//! macroblock grid; this decoder matches it to within two levels per sample (the donor's measured
//! accuracy against ffmpeg's integer IDCT), not bit for bit.

use super::tests::survives;
use super::*;
use crate::isobmff::{self, TrackKind};
use crate::{Movie, VideoError};

const SAMPLE: &[u8] = include_bytes!("../../tests/fixtures/vc3/dnxhr-lb-256x144-2f.mov");

/// ffmpeg's decoded Y, then Cb, then Cr at one point per macroblock (x = 16 mx + 5 / 8 mx + 3,
/// y = 16 my + 9), 16 x 9 points per plane, for each of the two frames. Hex.
const REFERENCE: [&str; 2] = [
    "8f358ea0309343d22929936e6a6aaaaa2020393b35303ad2292929936a6aaaaa5151519191919da39b27286a6a6aaaaa515151919191d2d2296c3b38516caaaa515151919191d2d22929296a6a636257515151919191d2d22929296c6a60aaaa515151919191d2d229292c78726daaaa515151919191d2d22929816a806aaaaa515151919191d2d22929296a6a6aaaaa69567464725f1310f0f08acdcaa4a6a6777764656a6e1011f0f0f093caa4a6a65a5a5b363613668cb0eaf3cacaa4a6a65a5a5b36360f1010f0d3eae6d5a9a6a65a5a5b36360f1010f0f0f0caccc39e745a5a5b36360f1010f0f0f0c5caa5a6a65a5a5b37360f1010f0f0daaeb697a6a65a5a5b36360f1010f0f0b0caa0a4a6a65a5a5b36360f1010f0f0f0cacaa4a6a6a29fa0725d8490926e6e4edfde0f10109c9c807b736890926e6e6e4bde0f1010f0f0f122229219121e616cdede0f1010f0f0f022229292926e4b648cb8001510f0f0f022229292926e6e6ededcf9e2edf0f0f022229292926e6e6ed2de14100ff0f0f022219292926e6e6996c20b1010f0f0f022229292926e6e49de810f1010f0f0f022229292926e6e6edede0f1010",
    "8f248e9e309d3cd2292974686a6aaaaa202a3b3b35323dd2292929966a6aaaaa5151519191919aa3932b296a6a6aaaaa515151919191d2d2316a3a3a6b6aaaaa515151919191d2d22929296a4f696058515151919191d2d22929296f6a63aaaa515151917e91d2d22929308b6e88aaaa515151919191d2d22929346a756aaaaa515151919191d2d22929296a6a6aaaaa8d57726272470f10f0f094cecaa4a6a6776f68676a660c10f0f0f090caa4a6a65a5a5b363538698fd8f0f1cacaa4a6a65a5a5b36360f1010cacdebe5cba5a6a65a5a5b36360f1010f0f0f0cad6c79c7b5a5a5b36360f1010f0f0f0c0ca9ca69f5a5a5b34820f1010f0f0d49aca8ca6a65a5a5b36360f1010f0f0b5caaea4a6a65a5a5b36360f1010f0f0f0cacaa4a6a692a4a2695da88f926e6e47ddde0f10109c92857d728392926e6e6e49de0f1010f0f0f12222221e145e6f6fdede0f1010f0f0f022229292922f42668cdf171010f0f0f022229292926e6e6ee0b7e5e6f5f0f0f022229292926e6e6ed0de11100df0f0f020829292926e6e616edc171010f0f0f022229292926e6e49dea20f1010f0f0f022229292926e6e6edede0f1010",
];

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn samples() -> Vec<Vec<u8>> {
    let file = isobmff::open(SAMPLE.to_vec()).unwrap();
    let t = file.track_of_kind(TrackKind::Video).unwrap();
    (0..file.tracks[t].samples.len()).map(|i| file.read_sample(SAMPLE, t, i).unwrap()).collect()
}

#[test]
fn the_sample_is_a_low_bandwidth_resolution_independent_stream() {
    let frames = samples();
    assert_eq!(frames.len(), 2);
    for f in &frames {
        let h = probe(f).unwrap();
        assert_eq!((h.cid, h.width, h.lines, h.bit_depth, h.chroma), (1274, 256, 144, 8, ChromaFormat::Yuv422));
        assert!(!h.interlaced && !h.alpha && !h.rgb_planes() && !h.vbr);
        assert_eq!(h.coding_unit_size(), Some(f.len() as u32));
        assert_eq!(&f[f.len() - 4..], &header::EOF_SIGNATURE);
    }
}

#[test]
fn decodes_to_within_two_levels_of_ffmpeg() {
    for (i, (data, reference)) in samples().iter().zip(REFERENCE).enumerate() {
        let want = hex(reference);
        assert_eq!(want.len(), 3 * 16 * 9);
        for opts in [DecodeOptions { threads: false, spec_dequant: false }, DecodeOptions::default()] {
            let f = decode_frame_with(data, &opts).unwrap();
            assert_eq!((f.width, f.height, f.chroma, f.bit_depth), (256, 144, ChromaFormat::Yuv422, 8));
            let mut worst = 0u16;
            for my in 0..9usize {
                for mx in 0..16usize {
                    let n = my * 16 + mx;
                    let got =
                        [f.y[(my * 16 + 9) * 256 + mx * 16 + 5], f.cb[(my * 16 + 9) * 128 + mx * 8 + 3], f.cr[(my * 16 + 9) * 128 + mx * 8 + 3]];
                    for (p, g) in got.into_iter().enumerate() {
                        let w = u16::from(want[p * 144 + n]);
                        worst = worst.max(g.abs_diff(w));
                        assert!(g.abs_diff(w) <= 2, "frame {i} plane {p} block ({mx},{my}): {g} against ffmpeg's {w}");
                    }
                }
            }
            println!("frame {i}: worst difference over the {} reference points is {worst}", 3 * 144);
        }
    }
}

#[test]
fn opens_as_a_movie_and_shows_the_picture() {
    let mut m = Movie::open(SAMPLE.to_vec()).unwrap();
    let info = m.info().clone();
    assert_eq!((info.codec.as_str(), info.codec_detail.as_str()), ("dnxhd", "VC-3 (LB, 8-bit)"));
    assert_eq!((info.width, info.height, info.frame_count, info.decodable, info.quicktime), (256, 144, 2, true, true));
    let f = m.frame(0).unwrap();
    assert_eq!((f.width, f.height, f.rgba.len()), (256, 144, 256 * 144 * 4));
    // a colour-bar test pattern: far from one flat colour, and opaque
    let distinct: std::collections::HashSet<[u8; 3]> = f.rgba.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
    assert!(distinct.len() > 100 && f.rgba.chunks_exact(4).all(|p| p[3] == 255));
    assert!(m.frame(1).unwrap().rgba != f.rgba);
    assert!(matches!(m.frame(2), Err(VideoError::OutOfRange)));
}

/// Real encoder output as the starting point of the hostile-input sweeps: its header carries
/// whatever the writer put in the fields our own encoder leaves at their defaults.
#[test]
fn damaged_copies_of_the_sample_never_panic() {
    let frames = samples();
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n.max(1) as u64) as usize
    };
    for (fi, frame) in frames.iter().enumerate() {
        for n in (0..frame.len()).step_by(frame.len() / 60) {
            survives(&frame[..n], &format!("frame {fi} cut at {n}"));
        }
        for p in 0..0x300 {
            for v in [0u8, 0xff, 0x80] {
                let mut d = frame.clone();
                d[p] = v;
                survives(&d, &format!("frame {fi} header byte {p:#x} = {v:#x}"));
            }
        }
        for i in 0..200 {
            let mut d = frame.clone();
            for _ in 0..1 + next(6) {
                let p = next(d.len());
                d[p] ^= 1 << next(8);
            }
            survives(&d, &format!("frame {fi} mutation {i}"));
        }
    }
}
