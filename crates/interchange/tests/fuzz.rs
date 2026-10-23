//! Property tests: damaged and random inputs to the importers, and random sessions through the
//! writers and back.

mod support;

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use soundcraft_interchange::{ExportOptions, Format, ImportOptions, MediaMode, export, import};
use soundcraft_model::{ChannelFormat, Fade, FadeShape, Session, TrackKind};
use soundcraft_time::SampleRate;
use support::*;

fn export_of(s: &Session, format: Format, small_sectors: bool, media: MediaMode) -> Vec<u8> {
    export(s, format, &ExportOptions { small_sectors, media, ..Default::default() }).expect("export").0
}

/// Valid files to damage: both formats, both sector sizes, linked and embedded media.
fn corpus() -> &'static [(Format, Vec<u8>)] {
    static CORPUS: OnceLock<Vec<(Format, Vec<u8>)>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let mut small = Session::new("Small", SampleRate::HZ_48000);
        let src = add_source(&mut small, "A", 1, 24_000, 1);
        let t = small.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("T"));
        place(&mut small, t, "a", src, 0, 0, 12_000);
        let demo = demo_like();
        vec![
            (Format::Aaf, export_of(&small, Format::Aaf, true, MediaMode::Link)),
            (Format::Aaf, export_of(&small, Format::Aaf, false, MediaMode::Embed)),
            (Format::Aaf, export_of(&demo, Format::Aaf, true, MediaMode::Link)),
            (Format::Omf, export_of(&small, Format::Omf, false, MediaMode::Link)),
            (Format::Omf, export_of(&small, Format::Omf, false, MediaMode::Embed)),
            (Format::Omf, export_of(&demo, Format::Omf, false, MediaMode::Link)),
        ]
    })
}

/// One change to a file's bytes.
#[derive(Debug, Clone)]
enum Edit {
    /// Set the byte at `at` (as a fraction of the length, in 1/65536ths) to `v`.
    Set(u16, u8),
    /// Flip a bit.
    Flip(u16, u8),
    /// Overwrite four bytes with a little- or big-endian value; the values that matter in these
    /// formats are sizes, counts and sector numbers.
    Word(u16, u32, bool),
    /// Delete `len` bytes.
    Cut(u16, u8),
    /// Insert `len` copies of `v`.
    Insert(u16, u8, u8),
    /// Copy a run of bytes over another place.
    Copy(u16, u16, u8),
    /// Cut the file.
    Truncate(u16),
}

fn edit() -> impl Strategy<Value = Edit> {
    let word =
        prop_oneof![Just(0u32), Just(1), Just(0xFFFF_FFFF), Just(0xFFFF_FFFE), Just(0x7FFF_FFFF), Just(0x8000_0000), Just(0xFFFF_FFFA), any::<u32>()];
    prop_oneof![
        4 => (any::<u16>(), any::<u8>()).prop_map(|(a, v)| Edit::Set(a, v)),
        4 => (any::<u16>(), 0u8..8).prop_map(|(a, b)| Edit::Flip(a, b)),
        4 => (any::<u16>(), word, any::<bool>()).prop_map(|(a, v, be)| Edit::Word(a, v, be)),
        1 => (any::<u16>(), any::<u8>()).prop_map(|(a, n)| Edit::Cut(a, n)),
        1 => (any::<u16>(), any::<u8>(), any::<u8>()).prop_map(|(a, v, n)| Edit::Insert(a, v, n)),
        1 => (any::<u16>(), any::<u16>(), any::<u8>()).prop_map(|(a, b, n)| Edit::Copy(a, b, n)),
        1 => any::<u16>().prop_map(Edit::Truncate),
    ]
}

fn pos(len: usize, frac: u16) -> usize {
    (len as u128 * u128::from(frac) / 65_536) as usize
}

fn apply(bytes: &mut Vec<u8>, e: &Edit) {
    if bytes.is_empty() {
        return;
    }
    let len = bytes.len();
    match *e {
        Edit::Set(a, v) => bytes[pos(len, a)] = v,
        Edit::Flip(a, b) => bytes[pos(len, a)] ^= 1 << b,
        Edit::Word(a, v, be) => {
            let at = pos(len, a);
            let w = if be { v.to_be_bytes() } else { v.to_le_bytes() };
            for (i, x) in w.iter().enumerate() {
                if let Some(b) = bytes.get_mut(at + i) {
                    *b = *x;
                }
            }
        }
        Edit::Cut(a, n) => {
            let at = pos(len, a);
            bytes.drain(at..(at + usize::from(n) + 1).min(len));
        }
        Edit::Insert(a, v, n) => {
            let at = pos(len, a);
            bytes.splice(at..at, std::iter::repeat_n(v, usize::from(n) + 1));
        }
        Edit::Copy(a, b, n) => {
            let from = pos(len, a);
            let to = pos(len, b);
            let run: Vec<u8> = bytes[from..(from + usize::from(n) + 1).min(len)].to_vec();
            for (i, x) in run.iter().enumerate() {
                if let Some(d) = bytes.get_mut(to + i) {
                    *d = *x;
                }
            }
        }
        Edit::Truncate(a) => bytes.truncate(pos(len, a)),
    }
}

fn import_quickly(bytes: &[u8], format: Format) -> bool {
    let start = Instant::now();
    let ok = import(bytes, format, &ImportOptions::default()).is_ok();
    assert!(start.elapsed() < Duration::from_secs(10), "an import of {} bytes took {:?}", bytes.len(), start.elapsed());
    ok
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 4000, failure_persistence: None, ..ProptestConfig::default() })]

    /// Valid files with a few edits: Ok or an error, never a panic or a hang.
    #[test]
    fn damaged_files_never_panic(which in 0usize..6, edits in prop::collection::vec(edit(), 1..6)) {
        let (format, original) = &corpus()[which % corpus().len()];
        let mut bytes = original.clone();
        for e in &edits {
            apply(&mut bytes, e);
        }
        import_quickly(&bytes, *format);
        // and a format guess from the damaged bytes is only a guess
        let _ = soundcraft_interchange::detect(&bytes, None);
    }

    /// Bytes that are nothing like a file; the same bytes behind each format's opening.
    #[test]
    fn arbitrary_bytes_never_panic(body in prop::collection::vec(any::<u8>(), 0..2048)) {
        for format in [Format::Aaf, Format::Omf] {
            import_quickly(&body, format);
        }
        let mut aaf = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        aaf.extend_from_slice(&body);
        import_quickly(&aaf, Format::Aaf);
        let mut omf = body.clone();
        omf.extend_from_slice(b"\x00\x00\x00\x00\x00\x00\x00\x00");
        import_quickly(&omf, Format::Omf);
    }
}

/// The edit decisions of one clip.
#[derive(Debug, Clone)]
struct ClipSpec {
    gap: i64,
    len: i64,
    offset: i64,
    gain_half_db: i32,
    fade_in: (u8, usize),
    fade_out: (u8, usize),
}

fn clip_spec() -> impl Strategy<Value = ClipSpec> {
    (0i64..30_000, 2i64..90_000, 0i64..40_000, -24i32..=24, (0u8..=100, 0usize..5), (0u8..=100, 0usize..5))
        .prop_map(|(gap, len, offset, gain_half_db, fade_in, fade_out)| ClipSpec { gap, len, offset, gain_half_db, fade_in, fade_out })
}

fn session_spec() -> impl Strategy<Value = Vec<Vec<ClipSpec>>> {
    prop::collection::vec(prop::collection::vec(clip_spec(), 0..6), 1..4)
}

/// What the files carry of a session's audio tracks: the tracks with clips, and no shape for a fade
/// that has no length.
fn carried(s: &Session) -> Vec<(String, Vec<ClipView>)> {
    let mut v: Vec<_> = views(s, TrackKind::Audio).into_iter().filter(|(_, c)| !c.is_empty()).collect();
    for (_, clips) in &mut v {
        for c in clips {
            for f in [&mut c.fade_in, &mut c.fade_out] {
                if f.0 == 0 {
                    f.1 = FadeShape::default();
                }
            }
        }
    }
    v
}

/// Fades of at most a quarter of the clip each, so the two never meet.
fn fade(spec: (u8, usize), len: i64) -> Fade {
    let shapes = FadeShape::ALL;
    Fade { len: len / 4 * i64::from(spec.0) / 100, shape: shapes[spec.1 % shapes.len()], curvature: 0.0 }
}

fn build(spec: &[Vec<ClipSpec>]) -> Session {
    let mut s = Session::new("Random", SampleRate::HZ_48000);
    let src = add_source(&mut s, "Src", 1, 140_000, 7);
    for (i, clips) in spec.iter().enumerate() {
        let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some(&format!("T{i}")));
        let mut at = 0;
        for c in clips {
            at += c.gap;
            let id = place(&mut s, t, "c", src, c.offset, at, c.len);
            let clip = clip_mut(&mut s, id);
            clip.gain_db = c.gain_half_db as f32 / 2.0;
            clip.fade_in = fade(c.fade_in, c.len);
            clip.fade_out = fade(c.fade_out, c.len);
            at += c.len;
        }
    }
    s
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, failure_persistence: None, ..ProptestConfig::default() })]

    /// A random session of non-overlapping clips with gains and fades survives either format:
    /// every clip comes back at the same place, with the same length, source offset, gain and fades.
    #[test]
    fn random_sessions_round_trip(spec in session_spec()) {
        prop_assume!(spec.iter().any(|t| !t.is_empty()));
        let s = build(&spec);
        let want = carried(&s);
        for format in [Format::Aaf, Format::Omf] {
            let (bytes, _) = export(&s, format, &ExportOptions::default()).expect("export");
            let opts = ImportOptions { sample_rate: Some(SR), ..Default::default() };
            let (back, _) = import(&bytes, format, &opts).expect("import");
            let got = carried(&back.session);
            prop_assert_eq!(&want, &got, "{:?}", format);
        }
    }
}
