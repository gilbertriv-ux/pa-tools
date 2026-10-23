//! VC-3 through [`Movie`]: MOV files made in process with the bundled encoder (no binary
//! fixtures), opened, labelled and decoded to RGBA the way the app does.

use super::tests::encoder;
use super::*;
use crate::convert::Matrix;
use crate::isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use crate::synth::frame_color;
use crate::{Movie, VideoError};
use std::io::Cursor;

/// A MOV with one video track of `samples` (each a whole VC-3 frame). The track says `track_w` x
/// `track_h`; `AVdh` is the resolution-independent profiles' sample entry.
fn mov(track_w: u16, track_h: u16, samples: &[Vec<u8>]) -> Vec<u8> {
    let mut mw = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let t = mw.add_track(TrackConfig::new(SampleEntry::dnx(FourCc(*b"AVdh"), track_w, track_h), 25)).unwrap();
    for data in samples {
        mw.write_sample(t, WriteSample { data, duration: 1, composition_offset: 0, is_sync: true }).unwrap();
    }
    mw.finish().unwrap().into_inner()
}

/// A frame of one Y'CbCr colour, given at 8 bits and scaled to `depth`.
fn solid(w: u32, h: u32, chroma: ChromaFormat, depth: u8, [y, cb, cr]: [u8; 3]) -> Frame {
    let mut f = Frame::new(w, h, chroma, depth, false);
    let s = depth - 8;
    f.y.fill(u16::from(y) << s);
    f.cb.fill(u16::from(cb) << s);
    f.cr.fill(u16::from(cr) << s);
    f
}

fn assert_near(f: &crate::Frame, rgb: [u8; 3], tol: i32, what: &str) {
    assert_eq!(f.rgba.len(), (f.width * f.height * 4) as usize);
    for px in f.rgba.as_chunks::<4>().0.iter().step_by(13) {
        for c in 0..3 {
            assert!((i32::from(px[c]) - i32::from(rgb[c])).abs() <= tol, "{what}: frame {} pixel {px:?} expected {rgb:?}", f.index);
        }
        assert_eq!(px[3], 255);
    }
}

/// Frames of `n` distinct colours through `profile`; returns the MOV.
fn colour_movie(profile: Profile, depth: u8, w: u32, h: u32, n: usize) -> Vec<u8> {
    let mut enc = encoder(profile, depth, w, h);
    let chroma = enc.config().chroma();
    let frames: Vec<Vec<u8>> = (0..n).map(|i| enc.encode(&solid(w, h, chroma, depth, frame_color(i))).unwrap()).collect();
    mov(w as u16, h as u16, &frames)
}

const FORBIDDEN: [&str; 4] = ["apple", "prores", "avid", "dnx"];

#[test]
fn profiles_open_with_a_neutral_label_and_decode_to_the_right_colours() {
    for (profile, depth, label) in [
        (Profile::Lb, 8, "VC-3 (LB, 8-bit)"),
        (Profile::Sq, 8, "VC-3 (SQ, 8-bit)"),
        (Profile::Hq, 8, "VC-3 (HQ, 8-bit)"),
        (Profile::Hqx, 10, "VC-3 (HQX, 10-bit)"),
        (Profile::Hqx, 12, "VC-3 (HQX, 12-bit)"),
        (Profile::R444, 10, "VC-3 (444, 10-bit)"),
        (Profile::R444, 12, "VC-3 (444, 12-bit)"),
    ] {
        let (w, h, n) = (64u32, 48u32, 5usize);
        let mut m = Movie::open(colour_movie(profile, depth, w, h, n)).unwrap();
        let info = m.info().clone();
        assert_eq!((info.codec.as_str(), info.codec_detail.as_str()), ("dnxhd", label));
        assert!(info.decodable && info.quicktime && !info.has_audio, "{label}");
        assert_eq!((info.width, info.height, info.frame_count), (w, h, n));
        let lower = info.codec_detail.to_ascii_lowercase();
        assert!(!FORBIDDEN.iter().any(|w| lower.contains(w)), "{label}");
        for i in 0..n {
            let [y, cb, cr] = frame_color(i);
            let expect = Matrix::new(true, false).rgb(y, cb, cr);
            let f = m.frame(i).unwrap();
            assert_eq!((f.index, f.width, f.height), (i, w, h));
            assert_near(&f, expect, 2, label);
        }
    }
}

#[test]
fn random_seeks_equal_sequential_decoding() {
    let bytes = colour_movie(Profile::Hq, 8, 64, 48, 8);
    let mut seq = Movie::open(bytes.clone()).unwrap();
    let sequential: Vec<_> = (0..8).map(|i| seq.frame(i).unwrap().rgba.clone()).collect();
    let mut m = Movie::open(bytes).unwrap();
    m.set_cache_frames(1);
    for &i in &[5usize, 0, 7, 3, 3, 6, 1, 2, 4, 7, 0] {
        assert_eq!(m.frame(i).unwrap().rgba, sequential[i], "frame {i}");
    }
    // by time as well
    assert_eq!(m.frame_at(3.0 / 25.0 + 0.001).unwrap().rgba, sequential[3]);
}

#[test]
fn thumbnails_are_decoded_and_scaled() {
    let mut m = Movie::open(colour_movie(Profile::Sq, 8, 96, 64, 6)).unwrap();
    let t = m.thumbnail_at(0.0, 32).unwrap();
    assert_eq!(t.height, 32);
    assert_near(&t, Matrix::new(true, false).rgb(frame_color(0)[0], frame_color(0)[1], frame_color(0)[2]), 2, "thumbnail");
    let all = m.thumbnails(4, 16);
    assert_eq!(all.len(), 4);
    assert!(all.iter().all(|(_, t)| t.as_ref().is_ok_and(|t| t.height == 16)));
}

/// Colour-model signalling: R'G'B' streams (planes G, B, R in video range) show their own colours,
/// and 4:4:4 Y'CbCr goes through the stream's matrix.
#[test]
fn rgb_and_ycbcr_444_streams_show_their_colours() {
    let (w, h, depth) = (48u32, 32u32, 10u8);
    let mut enc = encoder(Profile::R444, depth, w, h);
    let (r, g, b) = (200u8, 100u8, 50u8);
    let level = |c: u8| (((16.0 + f64::from(c) * 219.0 / 255.0) * f64::from(1u32 << (depth - 8))).round()) as u16;
    let mut rgb = Frame::new(w, h, ChromaFormat::Yuv444, depth, false);
    rgb.rgb = true;
    rgb.y.fill(level(g));
    rgb.cb.fill(level(b));
    rgb.cr.fill(level(r));
    let ycc = solid(w, h, ChromaFormat::Yuv444, depth, [150, 100, 180]);
    let frames = vec![enc.encode(&rgb).unwrap(), enc.encode(&ycc).unwrap()];
    assert!(probe(&frames[0]).unwrap().rgb_planes());
    let mut m = Movie::open(mov(w as u16, h as u16, &frames)).unwrap();
    assert_near(&m.frame(0).unwrap(), [r, g, b], 2, "rgb");
    assert_near(&m.frame(1).unwrap(), Matrix::new(true, false).rgb(150, 100, 180), 2, "ycbcr 444");
}

/// A field-coded interlaced frame is two coding units woven line by line (field 1 on the even lines).
fn field_coded(a: &[u8], b: &[u8], vbr: bool) -> Vec<u8> {
    let patch = |unit: &[u8], field: u8| {
        let mut u = unit.to_vec();
        u[5] = (u[5] & !3) | field | if vbr { 0x10 } else { 0 };
        u[0x22] |= 4; // interlaced source
        u[0x2c] &= 0x7f; // each field its own coding unit
        u
    };
    [patch(a, 2), patch(b, 3)].concat()
}

#[test]
fn field_coded_frames_are_woven() {
    let (w, lines) = (64u32, 32u32);
    let mut enc = encoder(Profile::Hq, 8, w, lines);
    let (top, bottom) = ([90u8, 100, 160], [190u8, 150, 90]);
    let a = enc.encode(&solid(w, lines, ChromaFormat::Yuv422, 8, top)).unwrap();
    let b = enc.encode(&solid(w, lines, ChromaFormat::Yuv422, 8, bottom)).unwrap();
    for vbr in [false, true] {
        let frame = field_coded(&a, &b, vbr);
        let hdr = probe(&frame).unwrap();
        assert!(hdr.interlaced && !hdr.frame_encoding && hdr.frame_lines() == 2 * lines);
        let mut m = Movie::open(mov(w as u16, (2 * lines) as u16, &[frame])).unwrap();
        let f = m.frame(0).unwrap();
        assert_eq!((f.width, f.height), (w, 2 * lines));
        let m709 = Matrix::new(true, false);
        for (row, line) in f.rgba.chunks_exact(f.width as usize * 4).enumerate() {
            let expect = m709.rgb(
                if row % 2 == 0 { top } else { bottom }[0],
                if row % 2 == 0 { top } else { bottom }[1],
                if row % 2 == 0 { top } else { bottom }[2],
            );
            let (pixels, _) = line.as_chunks::<4>();
            for px in pixels.iter().step_by(7) {
                for c in 0..3 {
                    assert!((i32::from(px[c]) - i32::from(expect[c])).abs() <= 2, "vbr {vbr} row {row}: {px:?} vs {expect:?}");
                }
            }
        }
        // the track must describe the woven picture, not one field
        let mut short = Movie::open(mov(w as u16, lines as u16, &[field_coded(&a, &b, vbr)])).unwrap();
        assert!(matches!(short.frame(0), Err(VideoError::Decode(why)) if why.contains("frame in a")));
    }
    // codec level: the same frame without the container
    let f = decode_frame(&field_coded(&a, &b, false)).unwrap();
    assert_eq!((f.height, f.interlace), (2 * lines, Interlace::TopFieldFirst));
}

#[test]
fn a_frame_that_is_not_the_tracks_size_is_refused() {
    let mut enc = encoder(Profile::Hq, 8, 64, 48);
    let frame = enc.encode(&solid(64, 48, ChromaFormat::Yuv422, 8, [100, 128, 128])).unwrap();
    // the track says 80 x 48
    let mut m = Movie::open(mov(80, 48, &[frame])).unwrap();
    assert!(m.info().decodable);
    assert!(matches!(m.frame(0), Err(VideoError::Decode(why)) if why.contains("64x48 frame in a 80x48 track")));
}

#[test]
fn a_tall_picture_is_labelled_from_a_header_longer_than_the_first_kilobyte() {
    // Above 1088 lines the header grows by 4 bytes per 16 lines: 2700 lines is 1044 bytes.
    let mut enc = encoder(Profile::Hq, 8, 64, 2700);
    let frame = enc.encode(&solid(64, 2700, ChromaFormat::Yuv422, 8, [120, 128, 128])).unwrap();
    assert!(probe(&frame).unwrap().header_size > 1024);
    let mut m = Movie::open(mov(64, 2700, &[frame])).unwrap();
    assert_eq!(m.info().codec_detail, "VC-3 (HQ, 8-bit)");
    assert!(m.info().decodable);
    assert_near(&m.frame(0).unwrap(), Matrix::new(true, false).rgb(120, 128, 128), 2, "tall");
}

#[test]
fn an_unknown_compression_id_opens_but_is_unsupported() {
    let mut enc = encoder(Profile::Hq, 8, 64, 48);
    let mut frame = enc.encode(&solid(64, 48, ChromaFormat::Yuv422, 8, [100, 128, 128])).unwrap();
    frame[0x28..0x2c].copy_from_slice(&1299u32.to_be_bytes());
    let mut m = Movie::open(mov(64, 48, &[frame])).unwrap();
    let info = m.info().clone();
    assert_eq!((info.codec.as_str(), info.codec_detail.as_str(), info.decodable), ("dnxhd", "VC-3", false));
    assert!(matches!(m.frame(0), Err(VideoError::Unsupported(why)) if why.contains("1299")));
    assert!(matches!(m.thumbnail_at(0.0, 20), Err(VideoError::Unsupported(_))));
    // the adapter itself says the same, were it reached
    let mut m = m.with_dnx_adapter();
    assert!(matches!(m.frame(0), Err(VideoError::Unsupported(why)) if why.contains("1299")));
}

#[test]
fn a_damaged_sample_is_an_error_not_a_panic_and_its_neighbours_still_decode() {
    let mut enc = encoder(Profile::Sq, 8, 64, 48);
    let good = enc.encode(&solid(64, 48, ChromaFormat::Yuv422, 8, [100, 128, 128])).unwrap();
    let mut bad = good.clone();
    bad.truncate(0x2a0); // the header and a few bytes: the scan lines are gone
    let mut m = Movie::open(mov(64, 48, &[good.clone(), bad, vec![0u8; 100], vec![0xff; 3000], good])).unwrap();
    assert!(m.info().decodable);
    for i in [1usize, 2, 3] {
        assert!(matches!(m.frame(i), Err(VideoError::Decode(_))), "sample {i}");
    }
    assert!(m.frame(0).is_ok() && m.frame(4).is_ok());
}

#[test]
fn a_track_with_a_garbage_first_sample_is_labelled_by_its_sample_entry_only() {
    let mut m = Movie::open(mov(64, 48, &[vec![0u8; 16]])).unwrap();
    assert_eq!((m.info().codec.as_str(), m.info().codec_detail.as_str(), m.info().decodable), ("dnxhd", "VC-3", true));
    assert!(matches!(m.frame(0), Err(VideoError::Decode(_))));
}

#[test]
fn hostile_headers_in_a_movie_are_refused_without_a_large_picture() {
    // A header that claims 8192 x 8192 in a 64 x 48 track, and one that claims a track-sized picture
    // but is far beyond the limit.
    let mut enc = encoder(Profile::Hq, 8, 64, 48);
    let frame = enc.encode(&solid(64, 48, ChromaFormat::Yuv422, 8, [100, 128, 128])).unwrap();
    for (w, lines) in [(8192u16, 8192u16), (0xffff, 0xffff), (16384, 16384)] {
        let mut d = frame.clone();
        d[0x18..0x1a].copy_from_slice(&lines.to_be_bytes());
        d[0x1a..0x1c].copy_from_slice(&w.to_be_bytes());
        d[0x16c..0x16e].copy_from_slice(&lines.div_ceil(16).to_be_bytes());
        let mut m = Movie::open(mov(64, 48, &[d])).unwrap();
        assert!(matches!(m.frame(0), Err(VideoError::Decode(_))), "{w}x{lines}");
    }
}
