//! Inputs that once made the AV1 decoder panic or misbehave, each kept as a regression test, plus the
//! recovery rules a damaged stream relies on. The `.frames` files hold `[u32 length][bytes]` per
//! temporal unit; they are libaom-encoded synthetic test pictures with a few header bits changed.
#![cfg(feature = "av1-decode")]

use soundcraft_video::av1::{Decoder, Error, Picture};
use std::panic::{AssertUnwindSafe, catch_unwind};

const GRAIN_POINTS: &[u8] = include_bytes!("fixtures/av1/regress-grain-points.frames");
const SCALED_REFERENCE: &[u8] = include_bytes!("fixtures/av1/regress-scaled-reference.frames");
const IVF_8: &[u8] = include_bytes!("fixtures/av1/av1-8bit-64x64-4f.ivf");
const IVF_10: &[u8] = include_bytes!("fixtures/av1/av1-10bit-64x64-4f.ivf");

fn frames_of(data: &[u8]) -> Vec<Vec<u8>> {
    let (mut out, mut pos) = (Vec::new(), 0);
    while let Some(h) = data.get(pos..pos + 4) {
        let n = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
        pos += 4;
        let end = pos.saturating_add(n).min(data.len());
        out.push(data[pos..end].to_vec());
        pos = end;
    }
    out
}

fn ivf_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Some(h) = data.get(6..8) else { return out };
    let mut pos = usize::from(u16::from_le_bytes([h[0], h[1]]));
    while let Some(hd) = data.get(pos..pos + 12) {
        let size = u32::from_le_bytes([hd[0], hd[1], hd[2], hd[3]]) as usize;
        pos += 12;
        let end = pos.saturating_add(size).min(data.len());
        out.push(data[pos..end].to_vec());
        pos = end;
    }
    out
}

/// Every temporal unit through a decoder; no panic, whatever the result. Returns the errors and pictures.
fn decode_all(threads: usize, frames: &[Vec<u8>]) -> (Vec<Error>, Vec<Picture>) {
    let mut dec = Decoder::with_threads(threads);
    let (mut errors, mut pics) = (Vec::new(), Vec::new());
    for (i, f) in frames.iter().enumerate() {
        match dec.decode_pts(f, i as i64) {
            Ok(p) => pics.extend(p),
            Err(e) => errors.push(e),
        }
    }
    match dec.flush_result() {
        Ok(p) => pics.extend(p),
        Err(e) => errors.push(e),
    }
    (errors, pics)
}

fn same(a: &Picture, b: &Picture) -> bool {
    (a.width, a.height, a.bit_depth) == (b.width, b.height, b.bit_depth) && a.planes == b.planes
}

#[test]
fn film_grain_scaling_points_that_do_not_increase_are_refused() {
    // The points were [x, x, ...]: a zero-width interpolation segment divided by zero.
    let frames = frames_of(GRAIN_POINTS);
    assert_eq!(frames.len(), 1);
    for threads in [1, 3] {
        let r = catch_unwind(AssertUnwindSafe(|| decode_all(threads, &frames)));
        let (errors, pics) = r.expect("the decoder must not panic");
        assert!(pics.is_empty());
        assert!(errors.iter().any(|e| matches!(e, Error::Invalid(m) if m.contains("scaling points"))), "{errors:?}");
    }
}

#[test]
fn a_reference_of_a_size_the_frame_cannot_predict_from_is_refused() {
    // A damaged header gave an inter frame a size more than twice or less than 1/16 of its reference's: the
    // prediction read past its scratch buffer.
    let frames = frames_of(SCALED_REFERENCE);
    for threads in [1, 3] {
        let r = catch_unwind(AssertUnwindSafe(|| decode_all(threads, &frames)));
        let (errors, _) = r.expect("the decoder must not panic");
        assert!(errors.iter().any(|e| matches!(e, Error::Invalid(m) if m.contains("cannot predict from"))), "{errors:?}");
    }
}

#[test]
fn a_damaged_unit_does_not_poison_the_next_good_key_frame() {
    let frames = ivf_frames(IVF_8);
    assert_eq!(frames.len(), 4);
    let (errors, good) = decode_all(1, &frames[..1]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(good.len(), 1);
    // Half of unit 1 (a partial tile group), a garbage unit, then the key frame again.
    let half = frames[1].len() / 2;
    let again: Vec<Vec<u8>> = vec![frames[0].clone(), frames[1][..half].to_vec(), vec![0xff; 40], frames[0].clone(), frames[0].clone()];
    for threads in [1, 3] {
        let (_, pics) = decode_all(threads, &again);
        let last = pics.last().expect("the key frame after the damage decodes");
        assert!(same(last, &good[0]), "the recovered key frame must be bit-exact");
        assert!(pics.len() >= 2);
    }
}

/// Split a temporal unit into OBUs (header byte, optional extension, leb128 size, payload).
fn obus(tu: &[u8]) -> Vec<(u8, &[u8])> {
    let (mut out, mut pos) = (Vec::new(), 0);
    while let Some(&h) = tu.get(pos) {
        let kind = (h >> 3) & 0xf;
        let mut p = pos + 1 + usize::from(h & 4 != 0);
        let (mut size, mut shift) = (0usize, 0);
        while let Some(&b) = tu.get(p) {
            p += 1;
            size |= usize::from(b & 0x7f) << shift;
            shift += 7;
            if b & 0x80 == 0 || shift > 28 {
                break;
            }
        }
        let end = (p + size).min(tu.len());
        out.push((kind, &tu[pos..end]));
        pos = end;
    }
    out
}

#[test]
fn a_new_picture_format_drops_the_old_references() {
    // 8-bit key frame, then the 10-bit sequence header with the 10-bit stream's first inter frame: that
    // frame refers to slots filled in another format and must be an error, not a mixed-up prediction.
    let f8 = ivf_frames(IVF_8);
    let f10 = ivf_frames(IVF_10);
    let seq10: Vec<u8> = obus(&f10[0]).iter().filter(|(k, _)| *k == 1).flat_map(|(_, b)| b.iter().copied()).collect();
    assert!(!seq10.is_empty());
    let mut inter = seq10.clone();
    inter.extend_from_slice(&f10[1]);
    let (_, ten) = decode_all(1, &f10[..1]);
    assert_eq!(ten.len(), 1);

    for threads in [1, 3] {
        let mut dec = Decoder::with_threads(threads);
        assert!(dec.decode_pts(&f8[0], 0).is_ok());
        let r = catch_unwind(AssertUnwindSafe(|| dec.decode_pts(&inter, 1)));
        assert!(r.expect("no panic").is_err(), "a prediction from another format's picture must be refused");
        // And the decoder is intact: the 10-bit key frame decodes exactly as it does on a fresh decoder.
        let pics = dec.decode_pts(&f10[0], 2).expect("the key frame decodes");
        let pics: Vec<Picture> = pics.into_iter().chain(dec.flush()).collect();
        assert!(pics.last().is_some_and(|p| same(p, &ten[0])));
    }
}

mod properties {
    use super::*;
    use proptest::prelude::*;

    /// The decoder turns a panic on a frame worker into this error; seeing it means a bug was caught
    /// only by the safety net.
    fn caught_by_safety_net(errors: &[Error]) -> bool {
        errors.iter().any(|e| matches!(e, Error::Invalid(m) if m.contains("internal decoder error")))
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// Bytes in, errors out: arbitrary data, and arbitrary data behind a valid temporal delimiter.
        #[test]
        fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..600), delimiter in any::<bool>()) {
            let mut unit = if delimiter { vec![0x12, 0x00] } else { Vec::new() };
            unit.extend(data);
            let (errors, _) = decode_all(1, &[unit]);
            prop_assert!(!caught_by_safety_net(&errors), "{errors:?}");
        }

        /// A real stream with a few bytes replaced anywhere: damaged headers and damaged tile data.
        #[test]
        fn damaged_streams_never_panic(edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..6), ten in any::<bool>(), threads in 1usize..4) {
            let mut frames = ivf_frames(if ten { IVF_10 } else { IVF_8 });
            for (at, value) in &edits {
                let unit = at.index(frames.len());
                if let Some(f) = frames.get_mut(unit) {
                    let p = at.index(f.len().max(1));
                    if let Some(b) = f.get_mut(p) {
                        *b = *value;
                    }
                }
            }
            let (errors, _) = decode_all(threads, &frames);
            prop_assert!(!caught_by_safety_net(&errors), "{errors:?}");
        }
    }
}
