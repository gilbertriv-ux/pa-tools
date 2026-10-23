//! AV1 end to end: the synthetic libaom fixtures (64x64, 4 frames, 8 and 10 bit, made from a test
//! pattern; see `ATTRIBUTION.md`) decode bit-exactly to the SHA-256 of ffmpeg's libdav1d output, play
//! through `Movie`, and hostile input never panics. Without the `av1-decode` feature the same files
//! open and say "unsupported in this build".

use soundcraft_video::{Movie, VideoError};

const MP4_8: &[u8] = include_bytes!("fixtures/av1/av1-8bit-64x64-4f.mp4");
const MP4_10: &[u8] = include_bytes!("fixtures/av1/av1-10bit-64x64-4f.mp4");
#[cfg(feature = "av1-decode")]
const IVF_8: &[u8] = include_bytes!("fixtures/av1/av1-8bit-64x64-4f.ivf");
#[cfg(feature = "av1-decode")]
const IVF_10: &[u8] = include_bytes!("fixtures/av1/av1-10bit-64x64-4f.ivf");

/// SHA-256 of the decoded output, from `ffmpeg -c:v libdav1d -i <ivf> -f rawvideo -pix_fmt yuv420p[10le]`
/// (README of the fixture set; libaom's own decode gives the same bytes).
#[cfg(feature = "av1-decode")]
const SHA_8: &str = "40a57f9181ba144228a4d7701268fc5d82ffedc7c570ae089ea54cb6941f28ac";
#[cfg(feature = "av1-decode")]
const SHA_10: &str = "b4dc4960c00d3d5a2cf9e712da0ec1742e714952b24bbd401a862637ba9274a3";

#[test]
fn the_files_open_in_every_build_and_say_what_they_are() {
    for (bytes, label) in [(MP4_8, "AV1 Main 8-bit 4:2:0"), (MP4_10, "AV1 Main 10-bit 4:2:0")] {
        let mut m = Movie::open(bytes).expect("a valid AV1 mp4 opens");
        let info = m.info().clone();
        assert_eq!((info.codec.as_str(), info.width, info.height, info.frame_count), ("av1", 64, 64, 4));
        assert_eq!(info.decodable, cfg!(feature = "av1-decode"));
        if cfg!(feature = "av1-decode") {
            assert_eq!(info.codec_detail, label);
            assert!(m.frame(0).is_ok(), "frame 0 decodes");
        } else {
            assert_eq!(info.codec_detail, "AV1");
            let Err(VideoError::Unsupported(why)) = m.frame(0) else { panic!("an AV1 frame in a build without the decoder must be Unsupported") };
            assert!(why.contains("unsupported in this build"), "{why}");
            assert!(matches!(m.thumbnail_at(0.0, 32), Err(VideoError::Unsupported(_))));
        }
    }
}

#[test]
fn damaged_av1_files_never_panic_in_any_build() {
    // Truncation at every length and a byte change at a stride, through the whole Movie path.
    for bytes in [MP4_8, MP4_10] {
        for n in (0..bytes.len()).step_by(7) {
            exercise(&bytes[..n]);
        }
        for p in (0..bytes.len()).step_by(5) {
            for v in [0u8, 0xff, 0x55] {
                let mut d = bytes.to_vec();
                d[p] = v;
                exercise(&d);
            }
        }
    }
}

/// Open a (possibly broken) movie and ask for everything; errors are fine, panics are not.
fn exercise(bytes: &[u8]) {
    if let Ok(mut m) = Movie::open(bytes.to_vec()) {
        for i in 0..m.info().frame_count.min(8) {
            let _ = m.frame(i);
        }
        let _ = m.thumbnails(3, 16);
    }
}

#[cfg(feature = "av1-decode")]
mod decode {
    use super::*;
    use soundcraft_video::av1::{Decoder, Error, Picture};
    use soundcraft_video::convert::{Matrix, planar_to_rgba, planar16_to_rgba};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- helpers -------------------------------------------------------------------------------

    /// SHA-256 (FIPS 180-4), so the tests compare against ffmpeg's published digests without a dependency.
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
        for block in msg.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, c) in block.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
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
    fn sha256_matches_the_published_vectors() {
        assert_eq!(sha256(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256(&[b'a'; 1000]), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }

    /// The frames of an IVF file (32-byte file header, 12-byte frame headers).
    pub(super) fn ivf_frames(data: &[u8]) -> Vec<Vec<u8>> {
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

    /// RGBA of a 64x64 4:2:0 picture the way `Movie` converts it (8-bit samples take the byte path).
    fn rgba_of(p: &Picture) -> Vec<u8> {
        let m = Matrix::for_code(p.matrix_coefficients, p.height, p.full_range);
        if p.bit_depth == 8 {
            let b = |v: &Vec<u16>| v.iter().map(|&s| s as u8).collect::<Vec<u8>>();
            planar_to_rgba(64, 64, &b(&p.planes[0]), 64, &b(&p.planes[1]), &b(&p.planes[2]), 32, 2, 2, &m)
        } else {
            planar16_to_rgba(64, 64, &p.planes[0], 64, &p.planes[1], &p.planes[2], 32, 2, 2, p.bit_depth, &m)
        }
    }

    fn decode_all(frames: &[Vec<u8>], threads: usize) -> Vec<Picture> {
        let mut dec = Decoder::with_threads(threads);
        let mut out = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            out.extend(dec.decode_pts(f, i as i64).expect("a valid frame decodes"));
        }
        out.extend(dec.flush_result().expect("flush"));
        out
    }

    /// Planes of every picture, cropped, in ffmpeg's `rawvideo` layout (bytes at 8 bit, little-endian u16 above).
    fn raw(pics: &[Picture]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in pics {
            for plane in &p.planes {
                for &s in plane {
                    if p.bit_depth == 8 {
                        out.push(s as u8);
                    } else {
                        out.extend_from_slice(&s.to_le_bytes());
                    }
                }
            }
        }
        out
    }

    // ---- bit-exactness -------------------------------------------------------------------------

    #[test]
    fn both_fixtures_decode_to_the_reference_digest_with_any_thread_count() {
        for (ivf, want, depth) in [(IVF_8, SHA_8, 8), (IVF_10, SHA_10, 10)] {
            let frames = ivf_frames(ivf);
            assert_eq!(frames.len(), 4);
            // `threads` > 1 runs frame workers when the crate's `threads` feature is on, else sequentially.
            for threads in [1, 2, 4] {
                let pics = decode_all(&frames, threads);
                assert_eq!(pics.len(), 4, "{depth}-bit, {threads} threads");
                assert!(pics.iter().all(|p| (p.width, p.height, p.bit_depth) == (64, 64, depth) && !p.draft));
                assert_eq!(sha256(&raw(&pics)), want, "{depth}-bit, {threads} threads");
            }
        }
    }

    #[test]
    fn the_mp4_samples_are_the_ivf_frames() {
        // Same bitstream in both containers: the first (key) frame decodes alone from either.
        let ivf = ivf_frames(IVF_8);
        let first = decode_all(&ivf[..1], 1);
        assert_eq!(first.len(), 1);
        let mut m = Movie::open(MP4_8).unwrap();
        let f = m.frame(0).unwrap();
        let p = &first[0];
        assert_eq!(f.rgba, rgba_of(p));
    }

    #[test]
    fn movie_frames_equal_the_verified_planes_and_do_not_depend_on_seek_order() {
        for (ivf, mp4) in [(IVF_8, MP4_8), (IVF_10, MP4_10)] {
            let pics = decode_all(&ivf_frames(ivf), 1);
            let want: Vec<Vec<u8>> = pics.iter().map(rgba_of).collect();
            // Sequential playback.
            let mut m = Movie::open(mp4).unwrap();
            for (i, w) in want.iter().enumerate() {
                assert_eq!(&m.frame(i).unwrap().rgba, w, "sequential frame {i}");
            }
            // Random seeks on fresh movies (no cache): each decodes forward from the key frame.
            for order in [[3usize, 1, 2, 0], [2, 3, 0, 1], [1, 0, 3, 2]] {
                let mut m = Movie::open(mp4).unwrap();
                for &i in &order {
                    assert_eq!(&m.frame(i).unwrap().rgba, &want[i], "seek order {order:?}, frame {i}");
                }
            }
            // Keyframe thumbnails, and times.
            let mut m = Movie::open(mp4).unwrap();
            let t = m.thumbnail_at(0.0, 16).unwrap();
            assert_eq!((t.width, t.height), (16, 16));
            assert_eq!(m.thumbnails(4, 16).len(), 4);
        }
    }

    #[test]
    fn eight_and_ten_bit_pictures_show_the_same_pattern() {
        // Both fixtures encode the same test pattern; the converted pictures agree closely.
        let (mut a, mut b) = (Movie::open(MP4_8).unwrap(), Movie::open(MP4_10).unwrap());
        let (fa, fb) = (a.frame(0).unwrap(), b.frame(0).unwrap());
        let diff: u64 = fa.rgba.iter().zip(&fb.rgba).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
        assert!(diff / fa.rgba.len() as u64 <= 6, "mean difference {}", diff / fa.rgba.len() as u64);
        assert!(fa.rgba.iter().any(|&v| v > 200) && fa.rgba.iter().any(|&v| v < 40), "the pattern is not blank");
    }

    // ---- hostile input -------------------------------------------------------------------------

    static PANICS: AtomicUsize = AtomicUsize::new(0);

    /// Count panics raised inside the decoder even when something catches them (worker threads
    /// turn them into errors; a caught panic is still a bug).
    pub(super) fn count_panics() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                if info.location().is_some_and(|l| l.file().replace('\\', "/").contains("/av1/")) {
                    PANICS.fetch_add(1, Ordering::SeqCst);
                    eprintln!("AV1 PANIC: {info}");
                }
                prev(info);
            }));
        });
    }

    fn run_all(frames: &[Vec<u8>], label: &str) {
        count_panics();
        let before = PANICS.load(Ordering::SeqCst);
        let res = catch_unwind(AssertUnwindSafe(|| {
            for threads in [1, 3] {
                let mut dec = Decoder::with_threads(threads);
                for (i, f) in frames.iter().enumerate() {
                    let _ = dec.decode_pts(f, i as i64);
                }
                let _ = dec.flush_result();
            }
        }));
        assert!(res.is_ok() && PANICS.load(Ordering::SeqCst) == before, "panic: {label}");
    }

    pub(super) struct Rng(pub u64);
    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        pub fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// Random-mutation rounds per seed (raise with `PATOOLS_FUZZ_ITERS` for a long hunt).
    pub(super) fn iters(default: usize) -> usize {
        std::env::var("PATOOLS_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(default)
    }

    #[test]
    fn truncated_frames_error_cleanly() {
        for ivf in [IVF_8, IVF_10] {
            let seed = ivf_frames(ivf);
            for fi in 0..seed.len() {
                for n in 0..seed[fi].len() {
                    let mut d = seed.clone();
                    d[fi].truncate(n);
                    run_all(&d, &format!("frame {fi} truncated to {n}"));
                }
            }
        }
    }

    #[test]
    fn every_header_byte_and_bit_can_be_flipped() {
        for ivf in [IVF_8, IVF_10] {
            let seed = ivf_frames(ivf);
            for fi in 0..seed.len() {
                for p in 0..seed[fi].len().min(64) {
                    for v in [0u8, 0xff, 0x80, 0x01, 0x7f] {
                        let mut d = seed.clone();
                        d[fi][p] = v;
                        run_all(&d, &format!("frame {fi} byte {p}={v:#x}"));
                    }
                    for b in 0..8 {
                        let mut d = seed.clone();
                        d[fi][p] ^= 1 << b;
                        run_all(&d, &format!("frame {fi} flip {p}.{b}"));
                    }
                }
            }
        }
    }

    #[test]
    fn random_mutations_never_panic() {
        for (k, ivf) in [IVF_8, IVF_10].into_iter().enumerate() {
            let seed = ivf_frames(ivf);
            let mut rng = Rng(0x9e3779b97f4a7c15 ^ (k as u64 + 1));
            for i in 0..iters(150) {
                let mut d = seed.clone();
                for _ in 0..1 + rng.below(6) {
                    let fi = rng.below(d.len());
                    if d[fi].is_empty() {
                        continue;
                    }
                    let p = rng.below(d[fi].len());
                    if rng.below(2) == 0 {
                        d[fi][p] ^= 1 << rng.below(8);
                    } else {
                        d[fi][p] = rng.next() as u8;
                    }
                }
                run_all(&d, &format!("seed {k} random {i}"));
            }
        }
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut rng = Rng(7);
        for len in [0usize, 1, 2, 3, 5, 9, 17, 64, 200, 1000] {
            for _ in 0..20 {
                let junk: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                run_all(&[junk], &format!("random bytes, length {len}"));
            }
        }
        // OBU headers with absurd sizes, and the forbidden bit.
        for h in [0x0au8, 0x12, 0x32, 0x8a, 0xff, 0x0e, 0x36] {
            for tail in [&[][..], &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f], &[0x05, 0, 0], &[0x80, 0x80, 0x80]] {
                let mut d = vec![h];
                d.extend_from_slice(tail);
                run_all(&[d], &format!("obu header {h:#x} tail {tail:?}"));
            }
        }
    }

    #[test]
    fn a_frame_without_its_sequence_header_is_an_error() {
        let frames = ivf_frames(IVF_8);
        // Strip the sequence header OBU (type 1) from the first temporal unit.
        let first = &frames[0];
        let mut stripped = Vec::new();
        let mut pos = 0;
        while pos < first.len() {
            let h = first[pos];
            let obu_type = (h >> 3) & 0xf;
            let mut p = pos + 1 + usize::from(h & 4 != 0);
            let (mut size, mut shift) = (0usize, 0);
            loop {
                let b = first[p];
                p += 1;
                size |= usize::from(b & 0x7f) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
            if obu_type != 1 {
                stripped.extend_from_slice(&first[pos..p + size]);
            }
            pos = p + size;
        }
        assert!(stripped.len() < first.len());
        let mut dec = Decoder::with_threads(1);
        let r = dec.decode_pts(&stripped, 0).and_then(|_| dec.flush_result());
        assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    }
}
