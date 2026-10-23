//! AAC export: `.m4a` and ADTS files from interleaved f32 audio.
//!
//! The files are checked three ways that share no code with the writers: the container parser of
//! `soundcraft-isobmff` (structure and the edit list), the clean-room AAC decoder of this crate
//! (decoded length after priming, loudness with the BS.1770 meter of `soundcraft-dsp`) and the
//! importer of `soundcraft-audio-io` (a different decoder, which also confirms the 1024-sample
//! alignment by cross-correlation).
//!
//! The tests that call ffmpeg are `#[ignore]`d; run them with `cargo test -p soundcraft-aac
//! --all-features -- --ignored`.

#![cfg(feature = "aac-encode")]

use std::io::{self, Cursor, Seek, SeekFrom, Write};

use soundcraft_aac::{
    AacOptions, AdtsWriter, AudioSpecificConfig, Decoder, ENCODER_BUILT, EXPORT_SAMPLE_RATES, Error, M4aWriter, Stats, bitrate_range, encode_adts,
    encode_adts_with, encode_m4a, encode_m4a_with, sample_rate_index, split_adts,
};
use soundcraft_dsp::meter::LoudnessMeter;
use soundcraft_isobmff::{CodecConfig, Edit, FourCc, Mp4File, TrackKind};

const PRIMING: usize = 1024;

// ---- signals ------------------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// Pink noise (Paul Kellet's filter on white noise), about 0.1 RMS.
fn pink(frames: usize, seed: u64) -> Vec<f32> {
    let mut r = Rng(seed);
    let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    (0..frames)
        .map(|_| {
            let w = r.next();
            b0 = 0.99886 * b0 + w * 0.0555179;
            b1 = 0.99332 * b1 + w * 0.0750759;
            b2 = 0.96900 * b2 + w * 0.1538520;
            b3 = 0.86650 * b3 + w * 0.3104856;
            b4 = 0.55000 * b4 + w * 0.5329522;
            b5 = -0.7616 * b5 - w * 0.0168980;
            let p = b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362;
            b6 = w * 0.115926;
            (p * 0.035) as f32
        })
        .collect()
}

/// A 1 kHz sine at -14 dBFS peak plus pink noise, per channel (the noise differs between channels).
fn test_signal(rate: u32, frames: usize, channels: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|c| {
            let noise = pink(frames, 0x9E37_79B9 + c as u64);
            (0..frames).map(|n| 0.2 * (2.0 * std::f64::consts::PI * 1000.0 * n as f64 / f64::from(rate)).sin() as f32 + noise[n]).collect()
        })
        .collect()
}

fn interleave(planar: &[Vec<f32>]) -> Vec<f32> {
    let frames = planar.first().map_or(0, Vec::len);
    (0..frames).flat_map(|n| planar.iter().map(move |c| c[n])).collect()
}

fn lufs(planar: &[Vec<f32>], rate: u32) -> f32 {
    let mut m = LoudnessMeter::new(rate as f32, planar.len());
    m.process(planar, planar[0].len());
    m.integrated_lufs()
}

/// Level of the `hz` component (Goertzel), as peak amplitude.
fn tone_level(x: &[f32], rate: u32, hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / f64::from(rate);
    let (mut re, mut im) = (0.0, 0.0);
    for (n, &v) in x.iter().enumerate() {
        re += f64::from(v) * (w * n as f64).cos();
        im += f64::from(v) * (w * n as f64).sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

/// Normalised correlation of `a` with `b` shifted by `lag` samples (`b[n + lag]`).
fn correlation(a: &[f32], b: &[f32], lag: isize) -> f64 {
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for (n, &x) in a.iter().enumerate().take(a.len().saturating_sub(2000)).skip(2000) {
        let m = n as isize + lag;
        let (x, y) = (f64::from(x), f64::from(b[m as usize]));
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    ab / (aa * bb).sqrt().max(1e-30)
}

// ---- decoding -------------------------------------------------------------------------------------

/// The access units of an `.m4a`, its parsed structure and the AudioSpecificConfig.
struct M4aInfo {
    mp4: Mp4File,
    asc: Vec<u8>,
    aus: Vec<Vec<u8>>,
}

fn open_m4a(file: &[u8]) -> M4aInfo {
    let mp4 = soundcraft_isobmff::open(file).unwrap();
    assert_eq!(mp4.tracks.len(), 1);
    assert_eq!(mp4.tracks[0].kind, TrackKind::Audio);
    let asc = match &mp4.tracks[0].entries[0].codec {
        CodecConfig::Aac(a) => a.asc.clone(),
        other => panic!("not an AAC track: {other:?}"),
    };
    let aus = (0..mp4.tracks[0].samples.len()).map(|i| mp4.read_sample(&file, 0, i).unwrap()).collect();
    M4aInfo { mp4, asc, aus }
}

/// Decodes access units with this crate's decoder: planar, every sample, no trimming.
fn decode_aus(asc: &[u8], aus: &[Vec<u8>]) -> Vec<Vec<f32>> {
    let mut dec = Decoder::new(asc).unwrap();
    let mut out: Vec<Vec<f32>> = Vec::new();
    for au in aus {
        let pcm = dec.decode(au).unwrap();
        assert!(pcm.iter().all(|c| c.len() == 1024));
        if out.is_empty() {
            out = vec![Vec::new(); pcm.len()];
        }
        for (o, p) in out.iter_mut().zip(&pcm) {
            o.extend_from_slice(p);
        }
    }
    out
}

fn trim(planar: &[Vec<f32>], skip: usize, take: usize) -> Vec<Vec<f32>> {
    planar.iter().map(|c| c[skip..skip + take].to_vec()).collect()
}

/// What a player plays: the decoded stream cut by the file's edit list.
fn play_m4a(info: &M4aInfo) -> Vec<Vec<f32>> {
    let edit = info.mp4.tracks[0].edits[0];
    let all = decode_aus(&info.asc, &info.aus);
    trim(&all, edit.media_time as usize, edit.segment_duration as usize)
}

// ---- cases ------------------------------------------------------------------------------------------

/// (sample rate, channels, bitrate)
const CASES: &[(u32, u16, u32)] = &[
    (48_000, 2, 128_000),
    (48_000, 1, 64_000),
    (44_100, 2, 192_000),
    (44_100, 1, 96_000),
    (32_000, 2, 96_000),
    (22_050, 1, 48_000),
    (16_000, 1, 32_000),
    (96_000, 2, 256_000),
    (88_200, 2, 256_000),
    (64_000, 1, 96_000),
    (24_000, 2, 64_000),
    (11_025, 1, 24_000),
    (8_000, 1, 16_000),
    (7_350, 1, 16_000),
];

fn case_frames(rate: u32) -> usize {
    rate as usize * 3 + 357
}

fn expected_aus(frames: usize) -> usize {
    frames.div_ceil(1024) + 1
}

// ---- .m4a ---------------------------------------------------------------------------------------------

#[test]
fn the_encoder_is_built_in_these_tests() {
    const { assert!(ENCODER_BUILT) };
}

#[test]
fn m4a_structure_and_edit_list() {
    for &(rate, ch, br) in CASES {
        let frames = case_frames(rate);
        let input = test_signal(rate, frames, usize::from(ch));
        let (file, stats) = encode_m4a_with(&interleave(&input), ch, rate, &AacOptions::new(br)).unwrap();
        let info = open_m4a(&file);
        let (mp4, track) = (&info.mp4, &info.mp4.tracks[0]);
        assert_eq!(mp4.major_brand, FourCc(*b"M4A "));
        assert!(mp4.compatible_brands.contains(&FourCc(*b"mp42")) && mp4.compatible_brands.contains(&FourCc(*b"isom")));
        assert!(!mp4.fragmented);
        // one tick of the movie clock is one audio sample: the edit list is exact
        assert_eq!((mp4.timescale, track.timescale), (rate, rate));
        assert_eq!(track.edits, vec![Edit { segment_duration: frames as u64, media_time: PRIMING as i64, media_rate: 0x1_0000 }], "{rate} Hz");
        assert_eq!((mp4.duration, track.tkhd_duration), (frames as u64, frames as u64));
        assert_eq!(track.samples.len(), expected_aus(frames));
        assert_eq!(stats.access_units as usize, track.samples.len());
        assert_eq!(track.duration, track.samples.len() as u64 * 1024);
        assert!(track.samples.iter().all(|s| s.duration == 1024 && s.is_sync));
        // moov first: the file plays while it downloads
        assert!(mp4.moov_offset < track.samples[0].offset);
        // the sample entry
        let CodecConfig::Aac(cfg) = &track.entries[0].codec else { panic!() };
        assert_eq!((cfg.object_type, cfg.sample_rate, u16::from(cfg.channel_config)), (2, rate, ch));
        assert_eq!((cfg.avg_bitrate, cfg.max_bitrate), (br, br));
        assert_eq!(usize::from(sample_rate_index(rate).unwrap()), usize::from(info.asc[0] & 7) << 1 | usize::from(info.asc[1] >> 7));
        assert_eq!(stats, Stats { frames_in: frames as u64, access_units: expected_aus(frames) as u64, non_finite: 0, clipped: 0 });
    }
}

#[test]
fn m4a_round_trip_keeps_length_channels_and_loudness() {
    let mut worst = 0.0f32;
    for &(rate, ch, br) in CASES {
        let frames = case_frames(rate);
        let input = test_signal(rate, frames, usize::from(ch));
        let file = encode_m4a(&interleave(&input), ch, rate, br).unwrap();
        let info = open_m4a(&file);
        let played = play_m4a(&info);
        assert_eq!(played.len(), usize::from(ch), "channel count");
        assert!(played.iter().all(|c| c.len() == frames), "{rate} Hz: {} frames played, {frames} written", played[0].len());
        assert!(played.iter().flatten().all(|v| v.is_finite()));
        let delta = lufs(&played, rate) - lufs(&input, rate);
        eprintln!("{rate:>6} Hz {ch} ch {br:>6} b/s: {frames} frames in, {} played, loudness delta {delta:+.3} LU", played[0].len());
        assert!(delta.abs() < 1.0, "{rate} Hz {ch} ch: {delta} LU");
        worst = worst.max(delta.abs());
        // the 1 kHz tone survives (at the low rates 1 kHz is still below the cutoff)
        let tone = tone_level(&played[0], rate, 1000.0);
        let want = tone_level(&input[0], rate, 1000.0);
        assert!((20.0 * (tone / want).log10()).abs() < 1.0, "{rate} Hz: tone {tone} vs {want}");
    }
    eprintln!("worst loudness delta over {} cases: {worst:.3} LU", CASES.len());
}

#[test]
fn another_decoder_agrees_and_confirms_the_priming_alignment() {
    // The importer's MP4 reader (symphonia) does not know the two highest AAC rates, 88.2 and 96 kHz;
    // those are covered by the ADTS test and by ffmpeg (`ffmpeg_reads_our_files_without_errors`).
    for &(rate, ch, br) in CASES.iter().filter(|c| c.0 < 88_200).take(8) {
        let frames = case_frames(rate);
        let input = test_signal(rate, frames, usize::from(ch));
        let file = encode_m4a(&interleave(&input), ch, rate, br).unwrap();
        // the importer decodes every access unit and does not apply the edit list, so the playable
        // part is cut here, with the numbers the file's edit list declares
        let (audio_info, buf) = soundcraft_audio_io::decode(&file, Some("m4a")).unwrap();
        assert_eq!((audio_info.sample_rate, buf.channels.len()), (rate, usize::from(ch)));
        assert_eq!(buf.channels[0].len(), expected_aus(frames) * 1024);
        let played = trim(&buf.channels, PRIMING, frames);
        // sample-accurate alignment: the correlation with the input peaks at lag 0 and not 1024 away
        for c in 0..usize::from(ch) {
            let zero = correlation(&input[c], &played[c], 0);
            for lag in [-1024isize, -64, -8, -2, -1, 1, 2, 8, 64, 1024] {
                if let Some(other) = correlation_checked(&input[c], &played[c], lag) {
                    assert!(zero > other, "{rate} Hz ch {c}: lag {lag} correlates {other:.4} vs {zero:.4} at 0");
                }
            }
            assert!(zero > 0.8, "{rate} Hz ch {c}: correlation {zero:.4}");
        }
        let delta = lufs(&played, rate) - lufs(&input, rate);
        assert!(delta.abs() < 1.0, "{rate} Hz: {delta} LU");
        // the two decoders produce the same audio
        let ours = play_m4a(&open_m4a(&file));
        for c in 0..usize::from(ch) {
            let err: f64 = ours[c].iter().zip(&played[c]).map(|(a, b)| f64::from(a - b).powi(2)).sum();
            let sig: f64 = ours[c].iter().map(|a| f64::from(*a).powi(2)).sum();
            assert!(10.0 * (sig / err.max(1e-30)).log10() > 50.0, "{rate} Hz ch {c}: decoders differ (error {err}, signal {sig})");
        }
    }
}

fn correlation_checked(a: &[f32], b: &[f32], lag: isize) -> Option<f64> {
    let reach = lag.unsigned_abs() + 2000;
    (a.len() > 2 * reach && b.len() > 2 * reach).then(|| correlation(a, b, lag))
}

#[test]
fn m4a_without_faststart_is_the_same_audio() {
    let (rate, ch) = (44_100u32, 2u16);
    let input = test_signal(rate, 30_000, 2);
    let x = interleave(&input);
    let fast = encode_m4a(&x, ch, rate, 128_000).unwrap();
    let mut w = M4aWriter::new(Cursor::new(Vec::new()), ch, rate, &AacOptions::new(128_000)).unwrap();
    w.write_interleaved(&x).unwrap();
    let (slow, stats) = w.finish().unwrap();
    let slow = slow.into_inner();
    assert_eq!(stats.frames_in, 30_000);
    let (a, b) = (open_m4a(&fast), open_m4a(&slow));
    assert!(b.mp4.moov_offset > b.mp4.tracks[0].samples[0].offset, "moov at the end");
    assert_eq!(a.aus, b.aus);
    assert_eq!((a.mp4.tracks[0].edits.clone(), a.asc), (b.mp4.tracks[0].edits.clone(), b.asc));
    assert_eq!(slow.len(), fast.len());
}

#[test]
fn tags_are_written_and_validated() {
    let x = interleave(&test_signal(48_000, 5000, 2));
    let mut opts = AacOptions::new(128_000);
    opts.metadata =
        vec![("\u{a9}nam".into(), "A title".into()), ("\u{a9}too".into(), "Test Tool 1.0".into()), ("\u{a9}ART".into(), "Anybody".into())];
    let (file, _) = encode_m4a_with(&x, 2, 48_000, &opts).unwrap();
    let mp4 = soundcraft_isobmff::open(file.as_slice()).unwrap();
    assert_eq!(mp4.metadata.title(), Some("A title"));
    assert_eq!(mp4.metadata.get("\u{a9}too"), Some("Test Tool 1.0"));
    assert_eq!(mp4.metadata.get("\u{a9}ART"), Some("Anybody"));
    // ADTS has no tags: the same options are accepted and ignored
    assert!(encode_adts_with(&x, 2, 48_000, &opts).is_ok());
    for bad in ["", "nam", "\u{a9}na", "\u{a9}name", "title", "\u{a9}n\u{0}m"] {
        let mut o = AacOptions::new(128_000);
        o.metadata = vec![(bad.into(), "x".into())];
        assert!(matches!(encode_m4a_with(&x, 2, 48_000, &o), Err(Error::InvalidMetadata(_))), "{bad:?}");
    }
    let mut o = AacOptions::new(128_000);
    o.metadata = vec![("\u{a9}cmt".into(), "x".repeat(1025))];
    assert!(matches!(encode_m4a_with(&x, 2, 48_000, &o), Err(Error::InvalidMetadata(_))));
    o.metadata = vec![("\u{a9}cmt".into(), "x".to_string()); 33];
    assert!(matches!(encode_m4a_with(&x, 2, 48_000, &o), Err(Error::InvalidMetadata(_))));
}

// ---- ADTS ---------------------------------------------------------------------------------------------

#[test]
fn adts_frames_are_valid_and_decode() {
    for &(rate, ch, br) in CASES {
        let frames = case_frames(rate);
        let input = test_signal(rate, frames, usize::from(ch));
        let (file, stats) = encode_adts_with(&interleave(&input), ch, rate, &AacOptions::new(br)).unwrap();
        let parts = split_adts(&file).unwrap();
        assert_eq!(parts.len(), expected_aus(frames));
        assert_eq!(stats.access_units as usize, parts.len());
        // the frames tile the file exactly
        assert_eq!(parts.iter().map(|(h, _)| h.frame_length).sum::<usize>(), file.len());
        let sf = sample_rate_index(rate).unwrap();
        for (h, payload) in &parts {
            assert_eq!((h.object_type, h.sf_index, h.channel_config, h.raw_data_blocks, h.header_length), (2, sf, ch as u8, 1, 7));
            assert_eq!(h.frame_length, payload.len() + 7);
            assert!(h.frame_length < 1 << 13);
        }
        let aus: Vec<Vec<u8>> = parts.iter().map(|(_, p)| p.to_vec()).collect();
        let asc = AudioSpecificConfig::lc(sf, ch as u8).to_bytes();
        let decoded = decode_aus(&asc, &aus);
        assert_eq!(decoded.len(), usize::from(ch));
        // a player starts ADTS with the priming and plays the whole padded tail
        let played = trim(&decoded, PRIMING, frames);
        let delta = lufs(&played, rate) - lufs(&input, rate);
        eprintln!("{rate:>6} Hz {ch} ch {br:>6} b/s ADTS: {} frames, loudness delta {delta:+.3} LU", parts.len());
        assert!(delta.abs() < 1.0, "{rate} Hz {ch} ch: {delta} LU");
        // the same access units as the .m4a
        let m4a = open_m4a(&encode_m4a(&interleave(&input), ch, rate, br).unwrap());
        assert_eq!(m4a.aus, aus, "ADTS payloads equal the .m4a samples");
        // and the importer decodes the stream
        let (info, buf) = soundcraft_audio_io::decode(&file, Some("aac")).unwrap();
        assert_eq!((info.sample_rate, buf.channels.len(), buf.channels[0].len()), (rate, usize::from(ch), parts.len() * 1024));
    }
}

#[test]
fn adts_and_m4a_are_deterministic() {
    let x = interleave(&test_signal(44_100, 20_000, 2));
    assert_eq!(encode_adts(&x, 2, 44_100, 128_000).unwrap(), encode_adts(&x, 2, 44_100, 128_000).unwrap());
    assert_eq!(encode_m4a(&x, 2, 44_100, 128_000).unwrap(), encode_m4a(&x, 2, 44_100, 128_000).unwrap());
}

// ---- streaming ------------------------------------------------------------------------------------------

#[test]
fn streaming_in_any_chunks_equals_one_shot() {
    let (rate, ch) = (44_100u32, 2u16);
    let x = interleave(&test_signal(rate, 20_011, 2));
    let one_adts = encode_adts(&x, ch, rate, 128_000).unwrap();
    let one_m4a = encode_m4a(&x, ch, rate, 128_000).unwrap();
    for chunk in [1usize, 7, 1023, 1024, 1025, 4096, 8191, 8193, 20_011] {
        let mut a = AdtsWriter::new(Vec::new(), ch, rate, &AacOptions::new(128_000)).unwrap();
        let mut m = M4aWriter::new(Cursor::new(Vec::new()), ch, rate, &AacOptions::new(128_000)).unwrap();
        for part in x.chunks(chunk * 2) {
            a.write_interleaved(part).unwrap();
            m.write_interleaved(part).unwrap();
        }
        assert_eq!(a.stats(), m.stats());
        let (adts, stats) = a.finish().unwrap();
        assert_eq!(adts, one_adts, "ADTS, chunks of {chunk} frames");
        assert_eq!(stats.frames_in, 20_011);
        let (m4a, _) = m.finish_faststart().unwrap();
        assert_eq!(m4a.into_inner(), one_m4a, "m4a, chunks of {chunk} frames");
    }
}

// ---- hostile input --------------------------------------------------------------------------------------------

#[test]
fn bad_parameters_are_errors() {
    let x = vec![0.1f32; 4096];
    let ok = |ch, rate, br| encode_m4a(&x, ch, rate, br).map(|_| ());
    assert_eq!(ok(2, 48_000, 128_000), Ok(()));
    // empty input, in every shape
    assert_eq!(encode_m4a(&[], 2, 48_000, 128_000), Err(Error::Empty));
    assert_eq!(encode_adts(&[], 1, 44_100, 64_000), Err(Error::Empty));
    // a trailing partial frame
    assert_eq!(encode_m4a(&[0.0; 3], 2, 48_000, 128_000), Err(Error::Misaligned { len: 3, channels: 2 }));
    assert_eq!(encode_adts(&x[..4095], 2, 48_000, 128_000), Err(Error::Misaligned { len: 4095, channels: 2 }));
    // channels: only 1 and 2
    for ch in [0u16, 3, 4, 6, 8, 9, 255, 256, 65_535] {
        assert_eq!(ok(ch, 48_000, 128_000), Err(Error::InvalidChannels(ch)));
        assert_eq!(encode_adts(&x, ch, 48_000, 128_000), Err(Error::InvalidChannels(ch)));
    }
    // sample rates: the 13 AAC rates only
    for rate in [0u32, 1, 7, 7_349, 7_351, 12_345, 44_099, 44_101, 47_999, 48_001, 88_199, 96_001, 176_400, 192_000, 384_000, u32::MAX] {
        assert_eq!(ok(2, rate, 128_000), Err(Error::InvalidSampleRate(rate)), "{rate}");
        assert_eq!(encode_adts(&x, 2, rate, 128_000), Err(Error::InvalidSampleRate(rate)));
    }
    // bitrates: from 8 kb/s per channel to the frame-size limit
    let (min, max) = bitrate_range(2, 48_000).unwrap();
    assert_eq!((min, max), (16_000, 576_000));
    for br in [0u32, 1, 1000, min - 1, max + 1, 1_000_000, u32::MAX] {
        assert_eq!(ok(2, 48_000, br), Err(Error::InvalidBitrate { bitrate: br, min, max }), "{br}");
        assert_eq!(encode_adts(&x, 2, 48_000, br), Err(Error::InvalidBitrate { bitrate: br, min, max }));
    }
    assert_eq!(ok(2, 48_000, min), Ok(()));
    assert_eq!(ok(2, 48_000, max), Ok(()));
    // the order of the checks: channels, then rate, then bitrate, then the data
    assert_eq!(encode_m4a(&[], 0, 0, 0), Err(Error::InvalidChannels(0)));
    assert_eq!(encode_m4a(&[], 2, 0, 0), Err(Error::InvalidSampleRate(0)));
    assert!(matches!(encode_m4a(&[], 2, 48_000, 0), Err(Error::InvalidBitrate { .. })));
    // a bitrate that is fine for one rate is too high for another
    assert!(ok(2, 48_000, 576_000).is_ok());
    assert!(matches!(ok(2, 8_000, 576_000), Err(Error::InvalidBitrate { max: 96_000, .. })));
}

#[test]
fn every_rate_takes_its_lowest_and_highest_bitrate() {
    for &rate in &EXPORT_SAMPLE_RATES {
        for ch in [1u16, 2] {
            let (min, max) = bitrate_range(ch, rate).unwrap();
            let input = test_signal(rate, 6000, usize::from(ch));
            for br in [min, (min + max) / 2, max] {
                let file = encode_m4a(&interleave(&input), ch, rate, br).unwrap();
                let played = play_m4a(&open_m4a(&file));
                assert!(played.iter().all(|c| c.len() == 6000 && c.iter().all(|v| v.is_finite())), "{rate} Hz {ch} ch {br} b/s");
            }
        }
    }
}

/// Signals an encoder is likely to choke on, as planar channel 0 of `frames` frames.
fn stress_signals(rate: u32, frames: usize) -> Vec<(&'static str, Vec<f32>)> {
    let mut r = Rng(0xC0FFEE);
    let white: Vec<f32> = (0..frames).map(|_| r.next() as f32).collect();
    vec![
        ("silence", vec![0.0; frames]),
        ("white noise at full scale", white.clone()),
        ("loud white noise, clipped", white.iter().map(|v| v * 50.0).collect()),
        ("dc at full scale", vec![1.0; frames]),
        ("dc at minus full scale", vec![-1.0; frames]),
        ("nyquist square wave", (0..frames).map(|n| if n % 2 == 0 { 1.0 } else { -1.0 }).collect()),
        ("impulses", (0..frames).map(|n| if n % 777 == 0 { 1.0 } else { 0.0 }).collect()),
        ("all NaN", vec![f32::NAN; frames]),
        ("NaN and infinities mixed in", (0..frames).map(|n| [0.3, f32::NAN, f32::INFINITY, f32::NEG_INFINITY][n % 4]).collect()),
        ("huge values", (0..frames).map(|n| if n % 2 == 0 { f32::MAX } else { f32::MIN }).collect()),
        ("subnormals", (0..frames).map(|n| f32::MIN_POSITIVE / (1 + n % 7) as f32).collect()),
        (
            "tone burst after silence",
            (0..frames).map(|n| if n > frames / 2 { (n as f32 * std::f32::consts::TAU * 1000.0 / rate as f32).sin() } else { 0.0 }).collect(),
        ),
    ]
}

#[test]
fn stress_signals_encode_at_every_rate_and_both_ends_of_the_bitrate_range() {
    for &rate in &EXPORT_SAMPLE_RATES {
        for ch in [1u16, 2] {
            let (min, max) = bitrate_range(ch, rate).unwrap();
            for (name, mono) in stress_signals(rate, 3000) {
                let planar: Vec<Vec<f32>> = (0..ch).map(|c| mono.iter().map(|v| if c == 1 { -v } else { *v }).collect()).collect();
                for br in [min, max] {
                    let ctx = format!("{name}, {rate} Hz, {ch} ch, {br} b/s");
                    let (file, stats) =
                        encode_m4a_with(&interleave(&planar), ch, rate, &AacOptions::new(br)).unwrap_or_else(|e| panic!("{ctx}: {e}"));
                    assert_eq!(stats.frames_in, 3000, "{ctx}");
                    let played = play_m4a(&open_m4a(&file));
                    assert!(played.iter().all(|c| c.len() == 3000 && c.iter().all(|v| v.is_finite())), "{ctx}");
                    let adts = encode_adts(&interleave(&planar), ch, rate, br).unwrap();
                    assert_eq!(split_adts(&adts).unwrap().len(), stats.access_units as usize, "{ctx}");
                }
            }
        }
    }
}

#[test]
fn sanitising_is_counted_and_keeps_the_audio_sane() {
    let mut x = interleave(&test_signal(48_000, 10_000, 2));
    x[10] = f32::NAN;
    x[11] = f32::INFINITY;
    x[12] = f32::NEG_INFINITY;
    x[13] = 5.0;
    x[14] = -1e30;
    let (file, stats) = encode_m4a_with(&x, 2, 48_000, &AacOptions::new(128_000)).unwrap();
    assert_eq!((stats.non_finite, stats.clipped), (3, 2));
    let played = play_m4a(&open_m4a(&file));
    assert!(played.iter().flatten().all(|v| v.is_finite() && v.abs() < 2.0));
}

#[test]
fn tiny_inputs_work_at_every_rate() {
    for &rate in &EXPORT_SAMPLE_RATES {
        for ch in [1u16, 2] {
            let (min, _) = bitrate_range(ch, rate).unwrap();
            for frames in [1usize, 2, 3, 100, 1023, 1024, 1025, 2047, 2048, 2049] {
                let x = vec![0.25f32; frames * usize::from(ch)];
                let file = encode_m4a(&x, ch, rate, min).unwrap();
                let info = open_m4a(&file);
                assert_eq!(info.aus.len(), expected_aus(frames), "{rate} Hz {frames} frames");
                assert_eq!(info.mp4.tracks[0].edits[0].segment_duration, frames as u64);
                assert!(play_m4a(&info).iter().all(|c| c.len() == frames));
                let adts = encode_adts(&x, ch, rate, min).unwrap();
                assert_eq!(split_adts(&adts).unwrap().len(), expected_aus(frames));
            }
        }
    }
}

#[test]
fn writers_reject_misuse_and_stay_usable() {
    let mut w = AdtsWriter::new(Vec::new(), 2, 48_000, &AacOptions::new(128_000)).unwrap();
    assert!(matches!(w.write_interleaved(&[0.0; 3]), Err(Error::Misaligned { .. })));
    // the refused call changed nothing
    assert_eq!(w.stats(), Stats::default());
    assert!(matches!(w.finish(), Err(Error::Empty)));
    let mut w = M4aWriter::new(Cursor::new(Vec::new()), 1, 44_100, &AacOptions::new(64_000)).unwrap();
    w.write_interleaved(&[]).unwrap();
    assert!(matches!(w.finish(), Err(Error::Empty)));
    let mut w = M4aWriter::new(Cursor::new(Vec::new()), 1, 44_100, &AacOptions::new(64_000)).unwrap();
    assert!(w.write_interleaved(&[0.1; 5000]).is_ok());
    assert!(w.write_interleaved(&[0.1; 5000]).is_ok());
    assert_eq!(w.stats().frames_in, 10_000);
    assert!(w.finish().is_ok());
}

/// A sink that fails after `budget` bytes.
struct Flaky {
    inner: Cursor<Vec<u8>>,
    budget: usize,
}

impl Write for Flaky {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.budget {
            return Err(io::Error::other("disk full"));
        }
        self.budget -= buf.len();
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Seek for Flaky {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn a_failing_sink_is_an_io_error_never_a_panic() {
    let x = interleave(&test_signal(44_100, 30_000, 2));
    let opts = AacOptions::new(128_000);
    let m4a_len = encode_m4a(&x, 2, 44_100, 128_000).unwrap().len();
    let adts_len = encode_adts(&x, 2, 44_100, 128_000).unwrap().len();
    let (mut failures, mut successes) = (0, 0);
    for budget in (0..40_000).step_by(331).chain([100_000]) {
        let m4a = (|| {
            let mut w = M4aWriter::new(Flaky { inner: Cursor::new(Vec::new()), budget }, 2, 44_100, &opts)?;
            w.write_interleaved(&x)?;
            w.finish()
        })();
        let adts = (|| {
            let mut w = AdtsWriter::new(Flaky { inner: Cursor::new(Vec::new()), budget }, 2, 44_100, &opts)?;
            w.write_interleaved(&x)?;
            w.finish()
        })();
        for (name, r, len) in [("m4a", m4a.map(|_| ()), m4a_len), ("adts", adts.map(|_| ()), adts_len)] {
            match r {
                // a sink with room for the whole file never fails (the .m4a also rewrites a few header bytes)
                Ok(()) => {
                    assert!(budget >= len, "{name}: succeeded with {budget} bytes of room for {len}");
                    successes += 1;
                }
                Err(Error::Io(m)) => {
                    assert!(m.contains("disk full"), "{m}");
                    assert!(budget < len + 64, "{name}: failed with {budget} bytes of room for {len}");
                    failures += 1;
                }
                Err(other) => panic!("{name}, budget {budget}: {other:?}"),
            }
        }
    }
    assert!(failures > 20 && successes >= 2, "{failures} failures, {successes} successes");
}

// ---- proptest ----------------------------------------------------------------------------------------------------

mod fuzz {
    use proptest::prelude::*;

    use super::*;

    fn cases(default: u32) -> u32 {
        std::env::var("SOUNDCRAFT_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    fn any_sample() -> impl Strategy<Value = f32> {
        prop_oneof![
            8 => -1.5f32..1.5,
            1 => any::<f32>(),
            1 => Just(f32::NAN),
            1 => Just(f32::INFINITY),
            1 => Just(f32::NEG_INFINITY),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases(24)))]

        /// Valid input of any shape encodes, parses, decodes to its own length and agrees with ADTS.
        #[test]
        fn valid_input_round_trips(
            rate_i in 0..EXPORT_SAMPLE_RATES.len(),
            ch in 1u16..=2,
            frames in 1usize..5000,
            position in 0.0f64..=1.0,
            amp in 0.0f32..6.0,
            seed in any::<u64>(),
            kind in 0u8..3,
        ) {
            let rate = EXPORT_SAMPLE_RATES[rate_i];
            let (min, max) = bitrate_range(ch, rate).unwrap();
            let br = min + ((max - min) as f64 * position) as u32;
            let mut r = Rng(seed | 1);
            let x: Vec<f32> = (0..frames * usize::from(ch))
                .map(|i| match kind {
                    0 => (r.next() as f32) * amp,
                    1 => ((i / usize::from(ch)) as f32 * 0.07).sin() * amp,
                    _ => if r.next() > 0.99 { amp } else { 0.0 },
                })
                .collect();
            let (file, stats) = encode_m4a_with(&x, ch, rate, &AacOptions::new(br)).unwrap();
            prop_assert_eq!(stats.frames_in as usize, frames);
            let info = open_m4a(&file);
            prop_assert_eq!(info.aus.len(), expected_aus(frames));
            let played = play_m4a(&info);
            prop_assert_eq!(played.len(), usize::from(ch));
            prop_assert!(played.iter().all(|c| c.len() == frames && c.iter().all(|v| v.is_finite())));
            let adts = encode_adts(&x, ch, rate, br).unwrap();
            let aus: Vec<Vec<u8>> = split_adts(&adts).unwrap().iter().map(|(_, p)| p.to_vec()).collect();
            prop_assert_eq!(aus, info.aus);
        }

        /// Arbitrary parameters and samples: an answer, never a panic.
        #[test]
        fn arbitrary_parameters_never_panic(
            ch in prop_oneof![any::<u16>(), 0u16..4],
            rate in prop_oneof![any::<u32>(), proptest::sample::select(EXPORT_SAMPLE_RATES.to_vec())],
            br in prop_oneof![any::<u32>(), 0u32..700_000],
            samples in proptest::collection::vec(any_sample(), 0..3000),
        ) {
            let m4a = encode_m4a(&samples, ch, rate, br);
            let adts = encode_adts(&samples, ch, rate, br);
            prop_assert_eq!(m4a.is_ok(), adts.is_ok());
            if let Ok(file) = m4a {
                prop_assert!(soundcraft_isobmff::open(file.as_slice()).is_ok());
            }
        }
    }
}

// ---- ffmpeg (manual) -------------------------------------------------------------------------------------------------

fn tool(name: &str) -> Option<String> {
    std::process::Command::new(name).arg("-version").output().ok().filter(|o| o.status.success()).map(|_| name.to_string())
}

#[test]
#[ignore = "needs ffmpeg and ffprobe installed (a test tool, never a dependency)"]
fn ffmpeg_reads_our_files_without_errors() {
    let (Some(ffmpeg), Some(ffprobe)) = (tool("ffmpeg"), tool("ffprobe")) else {
        panic!("ffmpeg/ffprobe not installed");
    };
    let dir = std::env::temp_dir().join(format!("soundcraft-aac-ffmpeg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for &(rate, ch, br) in CASES {
        let frames = case_frames(rate);
        let input = test_signal(rate, frames, usize::from(ch));
        let x = interleave(&input);
        let m4a = dir.join(format!("t-{ch}-{rate}-{br}.m4a"));
        let aac = dir.join(format!("t-{ch}-{rate}-{br}.aac"));
        std::fs::write(&m4a, encode_m4a(&x, ch, rate, br).unwrap()).unwrap();
        std::fs::write(&aac, encode_adts(&x, ch, rate, br).unwrap()).unwrap();
        for path in [&m4a, &aac] {
            // a clean decode
            let out = std::process::Command::new(&ffmpeg).args(["-v", "error", "-i"]).arg(path).args(["-f", "null", "-"]).output().unwrap();
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            assert!(out.status.success() && err.trim().is_empty(), "{path:?}: {err}");
            // the stream properties
            let probe = std::process::Command::new(&ffprobe)
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "stream=codec_name,profile,channels,sample_rate:format=format_name,duration",
                    "-of",
                    "default=nw=1",
                ])
                .arg(path)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&probe.stdout).to_string();
            assert!(probe.status.success() && probe.stderr.is_empty(), "ffprobe: {}", String::from_utf8_lossy(&probe.stderr));
            assert!(text.contains("codec_name=aac") && text.contains("profile=LC"), "{text}");
            assert!(text.contains(&format!("channels={ch}")) && text.contains(&format!("sample_rate={rate}")), "{text}");
            let duration: f64 = text.lines().find_map(|l| l.strip_prefix("duration=")).and_then(|v| v.parse().ok()).unwrap_or(0.0);
            eprintln!("{path:?}: ffprobe duration {duration:.6} s for {:.6} s of audio", frames as f64 / f64::from(rate));
            if path == &m4a {
                // the edit list is honoured: the duration is the input length
                assert!((duration - frames as f64 / f64::from(rate)).abs() < 0.0005, "m4a duration {duration}");
            }
            // decoded length and loudness through ffmpeg
            let pcm = std::process::Command::new(&ffmpeg)
                .args(["-v", "error", "-i"])
                .arg(path)
                .args(["-f", "f32le", "-ac", &ch.to_string(), "-ar", &rate.to_string(), "-"])
                .output()
                .unwrap();
            let (words, _) = pcm.stdout.as_chunks::<4>();
            let samples: Vec<f32> = words.iter().map(|b| f32::from_le_bytes(*b)).collect();
            let got = samples.len() / usize::from(ch);
            eprintln!("   ffmpeg decoded {got} frames (input {frames})");
            // ffmpeg applies the start of the edit list (it skips the priming) but plays the tail of the last
            // frame, so a decode is at most one frame longer than the input; ADTS has no edit list and is
            // played whole, priming included
            let skip = if path == &m4a {
                assert!(got >= frames && got < frames + 1024, "ffmpeg decoded {got} frames of {frames}");
                0
            } else {
                assert_eq!(got, expected_aus(frames) * 1024, "ffmpeg plays every access unit of the ADTS stream");
                PRIMING
            };
            let planar: Vec<Vec<f32>> = (0..usize::from(ch)).map(|c| samples.iter().skip(c).step_by(usize::from(ch)).copied().collect()).collect();
            let planar = trim(&planar, skip, frames);
            for c in 0..usize::from(ch) {
                let zero = correlation(&input[c], &planar[c], 0);
                assert!(
                    zero > 0.8 && zero > correlation(&input[c], &planar[c], 1) && zero > correlation(&input[c], &planar[c], -1),
                    "alignment {zero}"
                );
            }
            let delta = lufs(&planar, rate) - lufs(&input, rate);
            eprintln!("   ffmpeg-decoded loudness delta {delta:+.3} LU");
            assert!(delta.abs() < 1.0);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
