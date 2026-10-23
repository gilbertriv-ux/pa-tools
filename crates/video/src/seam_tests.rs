//! Tests of the codec seam: [`Movie`] over scripted [`GopDecoder`]s (reordering, open-GOP random
//! access, unreliable sync flags, decoder errors, hostile picture sizes), the H.264 adapter, and
//! the stubs for codecs whose decoders have not landed yet.

use crate::convert::{Matrix, Planes, Yuv, planar_to_rgba};
use crate::gop::{GopDecoder, Pic};
use crate::isobmff::{Av1Config, Brand, CodecConfig, FourCc, HevcConfig, Mp4Writer, SampleEntry, TrackConfig, VpcConfig, WriteSample, WriterOptions};
use crate::movie::{GopFactory, MAX_START_STEPS};
use crate::synth::h264_mp4;
use crate::{Frame, Movie, Result, VideoError};
use std::io::Cursor;
use std::sync::{Arc, Mutex};

/// Sample kinds, the first payload byte.
const KEY: u8 = b'K'; // IDR: random access, nothing before it is needed
const CRA: u8 = b'C'; // open-GOP random access point
const LEAD: u8 = b'L'; // leading picture of the preceding CRA: lost when decoding starts at that CRA
const INTER: u8 = b'P'; // anything else
const FAKE: u8 = b'X'; // flagged as a sync sample by the container, but not a real random-access point

/// (presentation index, kind) per sample, in decode order.
type Layout = Vec<(i64, u8)>;

/// Two GOPs with reordering; the second starts with a CRA whose leading pictures 5, 6 and 7 are
/// decoded after it and shown before it.
fn open_gop() -> Layout {
    [
        (0, KEY),
        (4, INTER),
        (2, INTER),
        (1, INTER),
        (3, INTER),
        (8, CRA),
        (6, LEAD),
        (5, LEAD),
        (7, LEAD),
        (12, INTER),
        (10, INTER),
        (9, INTER),
        (11, INTER),
    ]
    .to_vec()
}

/// An MP4 whose samples carry `[kind, decode index, 0, 0]`. The sample entry says HEVC, which has
/// no decoder yet; tests swap in a scripted one.
fn movie_bytes(layout: &Layout) -> Vec<u8> {
    let shift = layout.iter().enumerate().map(|(k, &(p, _))| k as i64 - p).max().unwrap_or(0).max(0);
    let mut mw = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = mw.add_track(TrackConfig::new(SampleEntry::hevc(Default::default(), 64, 48), 24)).unwrap();
    for (k, &(pts, kind)) in layout.iter().enumerate() {
        let sample = WriteSample {
            data: &[kind, k as u8, 0, 0],
            duration: 1,
            composition_offset: (pts - k as i64 + shift) as i32,
            is_sync: matches!(kind, KEY | CRA | FAKE),
        };
        mw.write_sample(t, sample).unwrap();
    }
    mw.finish().unwrap().into_inner()
}

#[derive(Default)]
struct Log {
    /// Decoders built.
    created: usize,
    /// Decode index of every sample fed to any decoder, in order.
    fed: Vec<u8>,
}

#[derive(Clone)]
struct Script {
    /// Pictures held back for reordering.
    depth: usize,
    checks_random_access: bool,
    /// Fail with a damaged-sample error on this decode index.
    fail_on: Option<u8>,
    /// Fail with an unsupported-stream error on this decode index.
    unsupported_on: Option<u8>,
    /// Size of the pictures produced.
    size: (u32, u32),
}

impl Default for Script {
    fn default() -> Self {
        Script { depth: 2, checks_random_access: false, fail_on: None, unsupported_on: None, size: (2, 2) }
    }
}

/// A decoder that reorders by presentation index, loses leading pictures of a CRA it started at,
/// and paints each picture a grey that identifies its sample.
struct Scripted {
    script: Script,
    log: Arc<Mutex<Log>>,
    held: Vec<(i64, u8)>,
    started: bool,
    start_pts: Option<i64>,
}

fn luma(id: u8) -> u8 {
    20 + (u16::from(id) * 2 % 200) as u8
}

fn picture(id: u8, (width, height): (u32, u32)) -> Yuv {
    let small = (width, height) == (2, 2);
    Yuv {
        width,
        height,
        planes: Planes::U8 { y: if small { vec![luma(id); 4] } else { Vec::new() }, u: vec![128; 1], v: vec![128; 1] },
        y_stride: 2,
        uv_stride: 1,
        cx: 2,
        cy: 2,
        bit_depth: 8,
        matrix: 1,
        full_range: false,
    }
}

impl Scripted {
    fn out(&mut self, flush: bool) -> Vec<Pic> {
        let mut pics = Vec::new();
        while self.held.len() > if flush { 0 } else { self.script.depth } {
            let Some(i) = (0..self.held.len()).min_by_key(|&i| self.held[i].0) else { break };
            let (pts, id) = self.held.remove(i);
            pics.push(Pic { pts, yuv: picture(id, self.script.size) });
        }
        pics
    }
}

impl GopDecoder for Scripted {
    fn decode(&mut self, au: &[u8], pts: i64) -> Result<Vec<Pic>> {
        let (kind, id) = (au[0], au[1]);
        self.log.lock().unwrap().fed.push(id);
        if self.script.fail_on == Some(id) {
            return Err(VideoError::Decode("mock damaged".into()));
        }
        if self.script.unsupported_on == Some(id) {
            return Err(VideoError::Unsupported("mock variant".into()));
        }
        if !self.started {
            self.started = true;
            if kind == CRA {
                self.start_pts = Some(pts);
            }
        }
        if kind == LEAD && self.start_pts.is_some_and(|s| pts < s) {
            return Ok(Vec::new());
        }
        self.held.push((pts, id));
        Ok(self.out(false))
    }

    fn flush(&mut self) -> Vec<Pic> {
        self.out(true)
    }

    fn checks_random_access(&self) -> bool {
        self.script.checks_random_access
    }

    fn is_random_access(&self, au: &[u8]) -> bool {
        au.first() != Some(&FAKE)
    }
}

fn scripted(layout: &Layout, script: Script) -> (Movie, Arc<Mutex<Log>>) {
    let log = Arc::new(Mutex::new(Log::default()));
    let shared = Arc::clone(&log);
    let factory: GopFactory = Arc::new(move || {
        shared.lock().unwrap().created += 1;
        Ok(Box::new(Scripted { script: script.clone(), log: Arc::clone(&shared), held: Vec::new(), started: false, start_pts: None })
            as Box<dyn GopDecoder>)
    });
    (Movie::open(movie_bytes(layout)).unwrap().with_test_gop(factory), log)
}

/// The decode index of the sample shown at presentation index `pts`.
fn id_at(layout: &Layout, pts: i64) -> u8 {
    layout.iter().position(|&(p, _)| p == pts).unwrap() as u8
}

fn assert_shows(f: &Frame, layout: &Layout, pts: i64) {
    assert_eq!(f.index as i64, pts);
    let want = Matrix::new(true, false).rgb(luma(id_at(layout, pts)), 128, 128);
    assert_eq!((f.width, f.height), (2, 2));
    for px in f.rgba.as_chunks::<4>().0 {
        for c in 0..3 {
            assert!((i32::from(px[c]) - i32::from(want[c])).abs() <= 1, "frame {pts}: {px:?} vs {want:?}");
        }
    }
}

fn fed(log: &Arc<Mutex<Log>>) -> Vec<u8> {
    log.lock().unwrap().fed.clone()
}

fn created(log: &Arc<Mutex<Log>>) -> usize {
    log.lock().unwrap().created
}

#[test]
fn sequential_playback_follows_presentation_order_with_one_decoder() {
    let layout = open_gop();
    let (mut m, log) = scripted(&layout, Script::default());
    assert_eq!(m.info().frame_count, 13);
    for pts in 0..13 {
        assert_shows(&m.frame(pts as usize).unwrap(), &layout, pts);
    }
    assert_eq!(created(&log), 1, "playing forward must not restart the decoder");
    assert_eq!(fed(&log), (0..13).collect::<Vec<u8>>(), "every sample is fed exactly once, in decode order");
}

#[test]
fn seeking_to_a_leading_picture_starts_before_the_cra() {
    let layout = open_gop();
    for lead in [5i64, 6, 7] {
        let (mut m, log) = scripted(&layout, Script::default());
        assert_shows(&m.frame(lead as usize).unwrap(), &layout, lead);
        assert_eq!(fed(&log).first(), Some(&0), "picture {lead} is shown before the CRA, so decoding starts at the earlier key frame");
    }
}

#[test]
fn seeking_past_the_cra_starts_at_the_cra() {
    let layout = open_gop();
    for pts in [8i64, 9, 12] {
        let (mut m, log) = scripted(&layout, Script::default());
        assert_shows(&m.frame(pts as usize).unwrap(), &layout, pts);
        assert_eq!(fed(&log).first(), Some(&5), "picture {pts} follows the CRA in presentation order");
    }
}

#[test]
fn random_seeks_equal_sequential_decoding() {
    let layout = open_gop();
    let (mut seq, _) = scripted(&layout, Script::default());
    let want: Vec<Vec<u8>> = (0..13).map(|i| seq.frame(i).unwrap().rgba.clone()).collect();
    let (mut m, _) = scripted(&layout, Script::default());
    m.set_cache_frames(1);
    for &i in &[12usize, 3, 6, 11, 0, 8, 5, 7, 9, 1, 12, 5] {
        assert!(m.frame(i).unwrap().rgba == want[i], "frame {i} differs after a seek");
    }
}

#[test]
fn thumbnails_snap_to_the_key_frame_that_shows_first() {
    let layout = open_gop();
    let (mut m, _) = scripted(&layout, Script::default());
    let t6 = m.frame_time(6).unwrap();
    assert_eq!(m.thumbnail_at(t6, 12).unwrap().index, 0, "a leading picture snaps to the previous GOP's key frame");
    let t9 = m.frame_time(9).unwrap();
    let th = m.thumbnail_at(t9, 12).unwrap();
    assert_eq!(th.index, 8, "a picture after the CRA snaps to the CRA");
    assert_shows(&th, &layout, 8);
    // Thumbnail decoding uses its own decoders and leaves the playback decoder alone.
    assert_shows(&m.frame(9).unwrap(), &layout, 9);
}

#[test]
fn a_false_sync_flag_is_stepped_over_when_the_decoder_checks() {
    let mut layout = open_gop();
    layout[5].1 = FAKE; // sample 5 is flagged sync but is not a random-access point
    let (mut m, log) = scripted(&layout, Script { checks_random_access: true, ..Script::default() });
    assert_shows(&m.frame(9).unwrap(), &layout, 9);
    assert_eq!(fed(&log).first(), Some(&0), "the false sync sample 5 is skipped for key frame 0");
    // Thumbnails step back the same way.
    let t9 = m.frame_time(9).unwrap();
    assert_eq!(m.thumbnail_at(t9, 12).unwrap().index, 0);
    // Without the check the container's flag is trusted.
    let (mut m, log) = scripted(&layout, Script::default());
    let _ = m.frame(9);
    assert_eq!(fed(&log).first(), Some(&5));
}

#[test]
fn the_start_search_is_bounded() {
    let n = 100;
    let layout: Layout = (0..n).map(|i| (i, FAKE)).collect();
    let (mut m, log) = scripted(&layout, Script { checks_random_access: true, ..Script::default() });
    assert_shows(&m.frame(n as usize - 1).unwrap(), &layout, n - 1);
    assert_eq!(fed(&log).first(), Some(&((n as usize - 1 - MAX_START_STEPS) as u8)));
    assert_eq!(created(&log), 1);
    // A sync flag on every sample whose presentation order is reversed cannot loop either.
    let rev: Layout = (0..n).map(|i| (n - 1 - i, KEY)).collect();
    let (mut m, log) = scripted(&rev, Script::default());
    let _ = m.frame(0);
    assert!(fed(&log).len() <= n as usize + 1);
}

#[test]
fn a_damaged_sample_is_skipped_and_reported_plainly() {
    let layout = open_gop();
    let (mut m, _) = scripted(&layout, Script { fail_on: Some(3), ..Script::default() }); // sample 3 shows picture 1
    assert_shows(&m.frame(0).unwrap(), &layout, 0);
    let err = m.frame(1).unwrap_err();
    assert_eq!(err, VideoError::Decode("mock damaged".into()), "the decoder's message, not wrapped twice");
    // Everything else still decodes.
    assert_shows(&m.frame(2).unwrap(), &layout, 2);
    assert_shows(&m.frame(4).unwrap(), &layout, 4);
}

#[test]
fn an_unsupported_stream_stops_decoding_at_once() {
    let layout = open_gop();
    let (mut m, log) = scripted(&layout, Script { unsupported_on: Some(2), ..Script::default() });
    assert_eq!(m.frame(0).unwrap_err(), VideoError::Unsupported("mock variant".into()));
    assert_eq!(fed(&log), vec![0, 1, 2], "no further samples are tried");
    // The next request builds a fresh decoder rather than trusting the stopped one.
    assert!(matches!(m.frame(0), Err(VideoError::Unsupported(_))));
    assert_eq!(created(&log), 2);
}

#[test]
fn hostile_picture_sizes_are_refused_without_allocating() {
    let layout = open_gop();
    let (mut m, _) = scripted(&layout, Script { size: (100_000, 100_000), ..Script::default() });
    assert_eq!(m.frame(0).unwrap_err(), VideoError::Decode("picture too large".into()));
    assert!(m.thumbnail_at(0.0, 12).is_err());
    let (mut m, _) = scripted(&layout, Script { size: (0, 0), ..Script::default() });
    assert!(matches!(m.frame(0), Err(VideoError::Decode(_))));
    let (mut m, _) = scripted(&layout, Script { size: (u32::MAX, u32::MAX), ..Script::default() });
    assert!(matches!(m.frame(0), Err(VideoError::Decode(_))));
}

#[test]
fn a_movie_can_move_to_a_decode_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<Movie>();
}

#[test]
fn the_h264_adapter_hands_over_the_decoders_own_planes() {
    let bytes = h264_mp4(3, 3).unwrap();
    let mp4 = crate::isobmff::open(bytes.clone()).unwrap();
    let Some(CodecConfig::Avc(c)) = mp4.tracks[0].codec() else { panic!("not H.264") };
    let avcc = c.to_bytes();
    let au = mp4.read_sample(&bytes, 0, 0).unwrap();

    let mut direct = crate::h264::Decoder::from_avcc(&avcc).unwrap();
    let mut a = direct.decode(&au, 7).unwrap();
    a.extend(direct.flush());
    let mut adapted = crate::adapt_h264::open(&avcc).unwrap();
    let mut b = adapted.decode(&au, 7).unwrap();
    b.extend(adapted.flush());
    assert_eq!((a.len(), b.len()), (1, 1));

    let (p, q) = (&a[0], &b[0]);
    assert_eq!(q.pts, 7);
    let Planes::U8 { y, u, v } = &q.yuv.planes else { panic!("H.264 pictures are 8-bit") };
    assert!(y == &p.y && u == &p.u && v == &p.v);
    assert_eq!((q.yuv.width, q.yuv.height, q.yuv.cx, q.yuv.cy, q.yuv.bit_depth), (64, 48, 2, 2, 8));
    assert!(!adapted.checks_random_access());
    let m = Matrix::for_code(p.color.matrix, p.height, p.color.full_range);
    assert!(q.yuv.to_rgba() == planar_to_rgba(64, 48, &p.y, p.y_stride, &p.u, &p.v, p.uv_stride, 2, 2, &m));

    assert!(matches!(crate::adapt_h264::open(&[1, 2, 3]), Err(VideoError::Decode(_))));
}

fn one_sample_movie(entry: SampleEntry) -> Movie {
    let mut mw = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = mw.add_track(TrackConfig::new(entry, 24)).unwrap();
    mw.write_sample(t, WriteSample { data: &[0u8; 16], duration: 1, composition_offset: 0, is_sync: true }).unwrap();
    Movie::open(mw.finish().unwrap().into_inner()).unwrap()
}

/// What an unsupported HEVC / AV1 track says, by build: the feature's decoder is not written yet, or
/// the build leaves it out.
fn hevc_phrase() -> &'static str {
    if cfg!(feature = "hevc-decode") { "not supported yet" } else { "unsupported in this build" }
}

fn av1_phrase() -> &'static str {
    if cfg!(feature = "av1-decode") { "not supported yet" } else { "unsupported in this build" }
}

#[test]
fn codecs_without_a_decoder_open_but_say_so() {
    let av1 = SampleEntry::video(FourCc(*b"av01"), CodecConfig::Av1(Av1Config::default()), 64, 48);
    let cases = [
        (SampleEntry::hevc(HevcConfig::default(), 64, 48), "hevc", "HEVC", hevc_phrase()),
        (av1, "av1", "AV1", av1_phrase()),
        (SampleEntry::dnx(FourCc(*b"AVdn"), 64, 48), "dnxhd", "VC-3", "not supported yet"),
    ];
    for (entry, id, label, phrase) in cases {
        let mut m = one_sample_movie(entry);
        let info = m.info().clone();
        assert_eq!((info.codec.as_str(), info.codec_detail.as_str(), info.decodable), (id, label, false));
        assert_eq!((info.width, info.height, info.frame_count), (64, 48, 1));
        let Err(VideoError::Unsupported(why)) = m.frame(0) else { panic!("{id} should be unsupported") };
        assert!(why.contains(label) && why.contains(phrase), "{why}");
        assert!(matches!(m.thumbnail_at(0.0, 20), Err(VideoError::Unsupported(_))));
    }
}

#[test]
fn the_codec_stubs_return_clean_errors() {
    assert!(matches!(crate::adapt_hevc::open(&[]), Err(VideoError::Unsupported(why)) if why.contains(hevc_phrase())));
    assert!(matches!(crate::adapt_av1::open(&[0x0a, 0x0b]), Err(VideoError::Unsupported(why)) if why.contains(av1_phrase())));
    assert!(crate::adapt_hevc::describe(&HevcConfig::default()).unsupported.is_some_and(|w| w.contains(hevc_phrase())));
    assert!(crate::adapt_av1::describe(&Av1Config::default()).unsupported.is_some_and(|w| w.contains(av1_phrase())));
    assert!(crate::adapt_dnx::describe(*b"AVdn", Some(&[0u8; 8])).unsupported.is_some());
    assert!(crate::adapt_dnx::describe(*b"AVdh", None).unsupported.is_some());
    let m = one_sample_movie(SampleEntry::dnx(FourCc(*b"AVdh"), 64, 48));
    assert!(matches!(crate::adapt_dnx::decode(&[0u8; 16], m.info()), Err(VideoError::Unsupported(_))));
    // The intra path of the movie reaches the adapter and passes its error on.
    let mut m = m.with_dnx_adapter();
    assert!(matches!(m.frame(0), Err(VideoError::Unsupported(why)) if why.contains("VC-3")));
}

/// Words that must never appear in anything the app shows about a codec.
const FORBIDDEN: [&str; 4] = ["apple", "prores", "avid", "dnx"];

#[test]
fn on_screen_codec_labels_name_no_third_party_product() {
    let mut entries = vec![
        SampleEntry::jpeg(64, 48),
        SampleEntry::hevc(HevcConfig::default(), 64, 48),
        SampleEntry::video(FourCc(*b"av01"), CodecConfig::Av1(Av1Config::default()), 64, 48),
        SampleEntry::video(FourCc(*b"vp09"), CodecConfig::Vp9(VpcConfig::default()), 64, 48),
        SampleEntry::dnx(FourCc(*b"AVdn"), 64, 48),
        SampleEntry::dnx(FourCc(*b"AVdh"), 64, 48),
    ];
    for f in [b"apco", b"apcs", b"apcn", b"apch", b"ap4h", b"ap4x"] {
        entries.push(SampleEntry::prores(FourCc(*f), 64, 48));
    }
    let mut labels = Vec::new();
    for entry in entries {
        let mut m = one_sample_movie(entry);
        labels.push(m.info().codec_detail.clone());
        if let Err(e) = m.frame(0) {
            labels.push(e.to_string());
        }
    }
    let h264 = Movie::open(h264_mp4(2, 2).unwrap()).unwrap();
    labels.push(h264.info().codec_detail.clone());
    for label in &labels {
        let lower = label.to_ascii_lowercase();
        assert!(!label.trim().is_empty() && !FORBIDDEN.iter().any(|w| lower.contains(w)), "label names a third-party product: {label:?}");
    }
    // ProRes is "PA ProRez", VC-3 is the standard's own name.
    assert!(labels.iter().any(|l| l == "PA ProRez 422 HQ"));
    assert!(labels.iter().any(|l| l == "PA ProRez 4444 XQ"));
    assert!(labels.iter().any(|l| l == "VC-3"));
}
