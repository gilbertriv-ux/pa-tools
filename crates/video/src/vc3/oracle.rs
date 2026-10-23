//! Oracle comparison against ffmpeg's VC-3 encoder and decoder. Ignored by default and skipped
//! when ffmpeg is not installed (set `PA_FFMPEG` to use another binary):
//!
//! ```text
//! cargo test -p soundcraft-video --lib vc3::oracle -- --ignored --nocapture
//! ```
//!
//! ffmpeg only ever runs as a separate program, in a scratch folder under the system temp
//! directory, which is removed afterwards. It makes the test streams (synthetic test patterns) and
//! its own decode is the reference. Nothing it produces is kept or committed (the one committed
//! sample is `tests/fixtures/vc3/`, tested in `fixture_tests.rs`).
//!
//! This decoder evaluates the exact inverse DCT where ffmpeg uses an integer one, so decode is not
//! bit-exact: every sample must be within two levels, and almost all within one.

use super::tests::{encoder, picture};
use super::*;
use crate::isobmff::{self, Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, TrackKind, WriteSample, WriterOptions};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

fn ffmpeg() -> Option<PathBuf> {
    let name = std::env::var_os("PA_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    match Command::new(&name).arg("-version").output() {
        Ok(o) if o.status.success() => Some(name.into()),
        _ => {
            println!("SKIPPED: ffmpeg is not installed (set PA_FFMPEG)");
            None
        }
    }
}

fn run(ff: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new(ff).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// A scratch folder for one test, removed when dropped.
struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("pa-tools-vc3-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch folder");
        Scratch(dir)
    }
    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Spec {
    name: &'static str,
    lavfi: String,
    frames: u32,
    /// Encoder arguments (bitrate / profile, pixel format, flags).
    enc: Vec<&'static str>,
    /// Raw pixel format of the reference decode.
    pix_fmt: &'static str,
}

fn spec(name: &'static str, lavfi: impl Into<String>, frames: u32, enc: &[&'static str], pix_fmt: &'static str) -> Spec {
    Spec { name, lavfi: lavfi.into(), frames, enc: enc.to_vec(), pix_fmt }
}

fn specs() -> Vec<Spec> {
    let t = |s: &str, r: u32| format!("testsrc2=s={s}:r={r}");
    let m = |s: &str, r: u32| format!("mandelbrot=s={s}:r={r}");
    let n = |s: &str, r: u32| {
        format!("testsrc2=s={s}:r={r},noise=alls=60:allf=t,lutyuv=y=clip(val\\,24\\,231):u=clip(val\\,24\\,231):v=clip(val\\,24\\,231)")
    };
    vec![
        // fixed-raster compression IDs
        spec("hd1237_testsrc", t("1920x1080", 25), 2, &["-b:v", "120M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1235_mandel", m("1920x1080", 25), 2, &["-b:v", "185M", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        spec("hd1238_noise", n("1920x1080", 25), 2, &["-b:v", "185M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1253_testsrc", t("1920x1080", 25), 2, &["-b:v", "36M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1250_noise", n("1280x720", 50), 2, &["-b:v", "90M", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        spec("hd1251_testsrc", t("1280x720", 50), 2, &["-b:v", "90M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1252_mandel", m("1280x720", 50), 2, &["-b:v", "60M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1258_testsrc", t("960x720", 50), 2, &["-b:v", "60M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1259_testsrc", t("1440x1080", 25), 2, &["-b:v", "84M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1242_interlaced", t("1920x1080", 25), 2, &["-flags", "+ildct", "-b:v", "120M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1241_interlaced", n("1920x1080", 25), 2, &["-flags", "+ildct", "-b:v", "185M", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        spec("hd1243_interlaced", m("1920x1080", 25), 2, &["-flags", "+ildct", "-b:v", "220M", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hd1244_interlaced", t("1440x1080", 25), 2, &["-flags", "+ildct", "-b:v", "120M", "-pix_fmt", "yuv422p"], "yuv422p"),
        // resolution-independent profiles
        spec("hr_lb_testsrc", t("1920x1080", 25), 2, &["-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hr_sq_mandel", m("1920x1080", 25), 2, &["-profile:v", "dnxhr_sq", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hr_hq_noise", n("1920x1080", 25), 2, &["-profile:v", "dnxhr_hq", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hr_hqx_mandel", m("1920x1080", 25), 2, &["-profile:v", "dnxhr_hqx", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        spec("hr_hqx_noise_odd", n("1000x562", 25), 2, &["-profile:v", "dnxhr_hqx", "-pix_fmt", "yuv422p10le"], "yuv422p10le"),
        spec("hr_444_testsrc", t("1280x720", 25), 2, &["-profile:v", "dnxhr_444", "-pix_fmt", "yuv444p10le"], "yuv444p10le"),
        spec("hr_444_rgb_mandel", m("640x360", 25), 2, &["-profile:v", "dnxhr_444", "-pix_fmt", "gbrp10le"], "gbrp10le"),
        spec("hr_hq_small", t("256x120", 25), 2, &["-profile:v", "dnxhr_hq", "-pix_fmt", "yuv422p"], "yuv422p"),
        spec("hr_sq_uhd", t("3840x2160", 25), 1, &["-profile:v", "dnxhr_sq", "-pix_fmt", "yuv422p"], "yuv422p"),
    ]
}

/// Every sample of the first video track of a movie file.
fn samples(mov: &[u8]) -> Vec<Vec<u8>> {
    let file = isobmff::open(mov.to_vec()).expect("movie");
    let t = file.track_of_kind(TrackKind::Video).expect("video track");
    (0..file.tracks[t].samples.len()).map(|i| file.read_sample(mov, t, i).expect("sample")).collect()
}

/// ffmpeg's decode of a movie file as raw planar samples (8-bit formats widened).
fn reference(ff: &Path, mov: &str, pix_fmt: &str, extra: &[&str]) -> Vec<u16> {
    let mut args = vec!["-i", mov];
    args.extend_from_slice(extra);
    args.extend(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"]);
    let raw = run(ff, &args);
    if pix_fmt.ends_with("le") {
        raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
    } else {
        raw.iter().map(|&b| u16::from(b)).collect()
    }
}

#[derive(Default, Debug)]
struct Stats {
    max: u32,
    sum: u64,
    n: u64,
    exact: u64,
    over1: u64,
}

impl Stats {
    fn add(&mut self, a: &[u16], b: &[u16]) {
        assert_eq!(a.len(), b.len());
        for (&x, &y) in a.iter().zip(b) {
            let d = u32::from(x.abs_diff(y));
            self.max = self.max.max(d);
            self.sum += u64::from(d);
            self.n += 1;
            self.exact += u64::from(d == 0);
            self.over1 += u64::from(d > 1);
        }
    }
}

/// Planes in the order of ffmpeg's planar raw output (R'G'B' frames hold G, B, R: the `gbrp` order).
fn planes(f: &Frame) -> Vec<u16> {
    [f.y.as_slice(), f.cb.as_slice(), f.cr.as_slice()].concat()
}

#[test]
#[ignore = "needs ffmpeg"]
fn decode_matches_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let dir = Scratch::new("decode");
    let mut worst = 0;
    for spec in specs() {
        let path = dir.file(&format!("{}.mov", spec.name));
        let frames = spec.frames.to_string();
        let mut args = vec!["-f", "lavfi", "-i", spec.lavfi.as_str(), "-frames:v", frames.as_str(), "-c:v", "dnxhd"];
        args.extend(spec.enc.iter().copied());
        args.extend(["-f", "mov", path.as_str()]);
        run(&ff, &args);
        let packets = samples(&std::fs::read(&path).unwrap());
        assert_eq!(packets.len(), spec.frames as usize, "{}", spec.name);
        let raw = reference(&ff, &path, spec.pix_fmt, &[]);
        let mut st = Stats::default();
        let mut off = 0;
        for (i, p) in packets.iter().enumerate() {
            let f = decode_frame_with(p, &DecodeOptions { threads: i % 2 == 0, ..Default::default() })
                .unwrap_or_else(|e| panic!("{} frame {i}: {e}", spec.name));
            let ours = planes(&f);
            st.add(&ours, &raw[off..off + ours.len()]);
            off += ours.len();
        }
        assert_eq!(off, raw.len(), "{}: frame size mismatch", spec.name);
        println!(
            "{:<20} cid {} max {} mean {:.5} exact {:.3}% beyond +-1: {}",
            spec.name,
            probe(&packets[0]).unwrap().cid,
            st.max,
            st.sum as f64 / st.n as f64,
            100.0 * st.exact as f64 / st.n as f64,
            st.over1
        );
        worst = worst.max(st.max);
        // what is left is ffmpeg's integer IDCT: at most about 50 samples per million at +-2
        assert!(st.over1 * 10_000 <= st.n, "{}: {} samples beyond +-1", spec.name, st.over1);
    }
    assert!(worst <= 2, "max error {worst}");
}

/// Streams from our encoder decode in ffmpeg (`-xerror`) to what our decoder makes of them.
#[test]
#[ignore = "needs ffmpeg"]
fn our_encoder_streams_decode_in_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let dir = Scratch::new("encode");
    for (profile, depth, pix_fmt, w, h) in [
        (Profile::Lb, 8u8, "yuv422p", 640u32, 360u32),
        (Profile::Sq, 8, "yuv422p", 640, 360),
        (Profile::Hq, 8, "yuv422p", 722, 486),
        (Profile::Hqx, 10, "yuv422p10le", 640, 360),
        (Profile::Hqx, 12, "yuv422p12le", 320, 180),
        (Profile::R444, 10, "yuv444p10le", 320, 180),
    ] {
        let mut enc = encoder(profile, depth, w, h);
        let chroma = enc.config().chroma();
        let coded: Vec<Vec<u8>> = (0..2).map(|s| enc.encode(&picture(w, h, chroma, depth, s)).unwrap()).collect();
        let mut mw = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
        let t = mw.add_track(TrackConfig::new(SampleEntry::dnx(FourCc(*b"AVdh"), w as u16, h as u16), 25)).unwrap();
        for data in &coded {
            mw.write_sample(t, WriteSample { data, duration: 1, composition_offset: 0, is_sync: true }).unwrap();
        }
        let path = dir.file(&format!("{}-{depth}.mov", profile.variant()));
        std::fs::write(&path, mw.finish().unwrap().into_inner()).unwrap();
        let raw = reference(&ff, &path, pix_fmt, &["-xerror"]);
        let mut st = Stats::default();
        let mut off = 0;
        for data in &coded {
            let ours = planes(&decode_frame(data).unwrap());
            st.add(&ours, &raw[off..off + ours.len()]);
            off += ours.len();
        }
        assert_eq!(off, raw.len());
        println!("{} {depth}-bit {w}x{h}: max {} exact {:.2}%", profile.name(), st.max, 100.0 * st.exact as f64 / st.n as f64);
        assert!(st.max <= 2 && st.over1 * 10_000 <= st.n, "{profile:?} {depth}-bit: max {} beyond1 {}", st.max, st.over1);
    }
}

/// The committed sample, compared with ffmpeg's decode of every sample (the unit test checks a
/// grid of points against stored values).
#[test]
#[ignore = "needs ffmpeg"]
fn committed_sample_matches_ffmpeg_everywhere() {
    let Some(ff) = ffmpeg() else { return };
    let dir = Scratch::new("sample");
    let path = dir.file("sample.mov");
    let bytes = include_bytes!("../../tests/fixtures/vc3/dnxhr-lb-256x144-2f.mov");
    std::fs::write(&path, bytes).unwrap();
    let raw = reference(&ff, &path, "yuv422p", &[]);
    let mut st = Stats::default();
    let mut off = 0;
    for p in samples(bytes) {
        let ours = planes(&decode_frame(&p).unwrap());
        st.add(&ours, &raw[off..off + ours.len()]);
        off += ours.len();
    }
    assert_eq!(off, raw.len());
    println!(
        "committed sample: {} samples, max {} mean {:.4} exact {:.2}% beyond +-1 {}",
        st.n,
        st.max,
        st.sum as f64 / st.n as f64,
        100.0 * st.exact as f64 / st.n as f64,
        st.over1
    );
    assert!(st.max <= 2);
}
