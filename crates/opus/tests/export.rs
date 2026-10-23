//! Ogg Opus export: the file structure (checked by an independent Ogg demuxer and a bit-by-bit CRC),
//! decoded length and loudness (checked with the clean-room Opus decoder and the BS.1770 meter of
//! `soundcraft-dsp`), streaming, hostile input.
//!
//! The tests that call ffmpeg are `#[ignore]`d; run them with `cargo test -p soundcraft-opus -- --ignored`.

mod common;

use soundcraft_dsp::meter::LoudnessMeter;
use soundcraft_ogg::{Codec, OpusTiming};
use soundcraft_opus::{Application, Error, MAX_BITRATE, OpusOggWriter, OpusOptions, encode_opus_ogg, encode_opus_ogg_with};

const PRE_SKIP: u64 = 120;

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

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
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

// ---- file inspection -------------------------------------------------------------------------------

/// Bit-by-bit CRC, independent of the writer's table.
fn crc_slow(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

#[derive(Debug)]
struct Page {
    flags: u8,
    granule: i64,
    serial: u32,
    sequence: u32,
    lacing: Vec<u8>,
    body_len: usize,
}

/// Parses every page, checking the capture pattern, the checksum and that the pages tile the file.
fn pages(file: &[u8]) -> Vec<Page> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < file.len() {
        assert_eq!(&file[pos..pos + 4], b"OggS", "capture pattern at {pos}");
        assert_eq!(file[pos + 4], 0, "stream structure version");
        let nseg = file[pos + 26] as usize;
        let lacing = file[pos + 27..pos + 27 + nseg].to_vec();
        let body_len: usize = lacing.iter().map(|&l| l as usize).sum();
        let end = pos + 27 + nseg + body_len;
        let mut page = file[pos..end].to_vec();
        let stored = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
        page[22..26].fill(0);
        assert_eq!(stored, crc_slow(&page), "CRC of the page at {pos}");
        out.push(Page {
            flags: file[pos + 5],
            granule: i64::from_le_bytes(file[pos + 6..pos + 14].try_into().unwrap()),
            serial: u32::from_le_bytes(file[pos + 14..pos + 18].try_into().unwrap()),
            sequence: u32::from_le_bytes(file[pos + 18..pos + 22].try_into().unwrap()),
            lacing,
            body_len,
        });
        pos = end;
    }
    out
}

/// Everything the tests need to know about a file, from the donor demuxer.
struct Parsed {
    channels: usize,
    head: Vec<u8>,
    tags: Vec<u8>,
    packets: Vec<Vec<u8>>,
    timing: OpusTiming,
}

fn parse(file: &[u8]) -> Parsed {
    let ogg = soundcraft_ogg::open(file).expect("demux");
    assert!(ogg.warnings.is_empty(), "demux warnings: {:?}", ogg.warnings);
    assert_eq!(ogg.streams.len(), 1);
    let s = ogg.stream_of(Codec::Opus).expect("an Opus stream");
    let st = &ogg.streams[s];
    assert!(st.saw_eos, "the stream ends with an EOS page");
    assert_eq!(st.headers.len(), 2);
    let head = st.headers[0].clone();
    let pre_skip = u32::from(u16::from_le_bytes([head[10], head[11]]));
    let timing = OpusTiming::of(st, pre_skip);
    let packets = (0..st.packets.len()).map(|i| ogg.read_packet(file, s, i).unwrap()).collect();
    Parsed { channels: head[9] as usize, head, tags: st.headers[1].clone(), packets, timing }
}

/// Decodes the file with the clean-room decoder and trims the end as the final granule says.
fn decode(file: &[u8]) -> Vec<Vec<f32>> {
    let p = parse(file);
    let mut planar = common::decode(&p.head, &p.packets);
    for ch in &mut planar {
        ch.truncate(p.timing.total as usize);
    }
    planar
}

fn expected_frames(frames_in: usize, rate: u32) -> usize {
    ((frames_in as u128 * 48_000 + u128::from(rate) / 2) / u128::from(rate)) as usize
}

/// Structure checks that hold for every file we write.
fn check_structure(file: &[u8], channels: usize, rate: u32, frames_in: usize) {
    let pg = pages(file);
    assert!(pg.len() >= 3);
    assert_eq!(pg[0].flags & 0x02, 0x02, "first page is BOS");
    assert!(pg[1..].iter().all(|p| p.flags & 0x02 == 0), "only the first page is BOS");
    assert_eq!(pg.last().unwrap().flags & 0x04, 0x04, "last page is EOS");
    assert!(pg[..pg.len() - 1].iter().all(|p| p.flags & 0x04 == 0), "only the last page is EOS");
    assert!(pg.iter().all(|p| p.flags & 0x01 == 0), "no packet is split across pages");
    assert!(pg.iter().enumerate().all(|(i, p)| p.sequence == i as u32), "page numbers count up from 0");
    assert!(pg.iter().all(|p| p.serial == pg[0].serial));
    assert!(pg.iter().all(|p| p.lacing.len() <= 255 && p.body_len <= 255 * 255));
    assert_eq!(pg[0].lacing, vec![19], "OpusHead is alone on the first page");
    assert_eq!((pg[0].granule, pg[1].granule), (0, 0), "header pages have granule 0");
    let granules: Vec<i64> = pg[2..].iter().map(|p| p.granule).collect();
    assert!(granules.windows(2).all(|w| w[0] < w[1]), "granule positions increase: {granules:?}");

    let out_frames = expected_frames(frames_in, rate);
    let wanted = out_frames as u64 + PRE_SKIP;
    assert_eq!(*granules.last().unwrap(), wanted as i64, "the last granule is pre-skip + duration");

    let p = parse(file);
    assert_eq!(p.channels, channels);
    assert_eq!(&p.head[..8], b"OpusHead");
    assert_eq!(p.head[8], 1, "version");
    assert_eq!(u16::from_le_bytes([p.head[10], p.head[11]]) as u64, PRE_SKIP);
    assert_eq!(u32::from_le_bytes([p.head[12], p.head[13], p.head[14], p.head[15]]), rate, "input rate");
    assert_eq!(&p.head[16..], &[0, 0, 0], "gain 0, mapping family 0");
    assert_eq!(&p.tags[..8], b"OpusTags");
    assert_eq!(p.packets.len() as u64, wanted.div_ceil(960), "packet count covers pre-skip + duration");
    assert_eq!(p.timing.total as usize, out_frames, "playable length per RFC 7845");
    for (i, packet) in p.packets.iter().enumerate() {
        let toc = packet[0];
        assert_eq!(usize::from(toc >> 2 & 1) + 1, channels, "TOC stereo flag of packet {i}");
        assert_eq!(toc & 3, 0, "one frame per packet");
        assert!(packet.len() <= 1276);
    }
}

// ---- round trips -----------------------------------------------------------------------------------

/// Encodes, checks the structure, decodes, and returns (decoded planar, input planar).
fn round_trip(rate: u32, seconds: f64, channels: usize, bitrate: u32) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let frames = (f64::from(rate) * seconds) as usize;
    let input = test_signal(rate, frames, channels);
    let file = encode_opus_ogg(&interleave(&input), channels as u16, rate, bitrate).unwrap();
    check_structure(&file, channels, rate, frames);
    let decoded = decode(&file);
    assert_eq!(decoded.len(), channels);
    for ch in &decoded {
        assert_eq!(ch.len(), expected_frames(frames, rate), "decoded length");
    }
    (decoded, input)
}

#[test]
fn stereo_48k_round_trip_keeps_length_channels_loudness_and_waveform() {
    let (dec, input) = round_trip(48_000, 4.0, 2, 128_000);
    let (a, b) = (lufs(&input, 48_000), lufs(&dec, 48_000));
    eprintln!("stereo 48 kHz 128 kb/s: input {a:.2} LUFS, decoded {b:.2} LUFS, delta {:.3} LU", b - a);
    assert!((b - a).abs() < 1.0, "loudness moved by {:.2} LU", b - a);
    // the 1 kHz tone survives at its level, and the waveform lines up with no residual delay
    for c in 0..2 {
        let (snr, delay) = common::snr_search(&input[c], &dec[c], 4);
        eprintln!("channel {c}: SNR {snr:.1} dB at residual delay {delay}");
        assert_eq!(delay, 0, "the pre-skip removes all of the encoder delay");
        assert!(snr > 12.0, "channel {c}: SNR {snr:.1} dB");
        let t = tone_level(&dec[c][4800..], 48_000, 1000.0);
        assert!((t - 0.2).abs() < 0.02, "1 kHz level {t}");
    }
}

#[test]
fn every_input_rate_gives_the_right_duration_and_loudness() {
    for rate in [8_000u32, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000] {
        let (dec, input) = round_trip(rate, 1.5, 1, 64_000);
        let (a, b) = (lufs(&input, rate), lufs(&dec, 48_000));
        eprintln!("mono {rate:>6} Hz 64 kb/s: delta {:+.3} LU, 1 kHz level {:.3}", b - a, tone_level(&dec[0][4800..], 48_000, 1000.0));
        assert!((b - a).abs() < 1.0, "{rate} Hz: loudness moved by {:.2} LU", b - a);
        let t = tone_level(&dec[0][4800..], 48_000, 1000.0);
        assert!((t - 0.2).abs() < 0.03, "{rate} Hz: 1 kHz level {t}");
    }
}

#[test]
fn bitrate_ladder_stays_within_a_lu() {
    for (ch, rate) in [(1usize, 32_000u32), (1, 96_000), (2, 64_000), (2, 192_000), (2, 510_000)] {
        let (dec, input) = round_trip(48_000, 2.0, ch, rate);
        let (a, b) = (lufs(&input, 48_000), lufs(&dec, 48_000));
        eprintln!("{ch} ch {rate:>6} b/s: delta {:+.3} LU", b - a);
        assert!((b - a).abs() < 1.0, "{ch} ch at {rate}: loudness moved by {:.2} LU", b - a);
    }
}

#[test]
fn speech_application_round_trips_at_low_rates() {
    let frames = 48_000 * 2;
    let sp = common::speech(frames, 5);
    let mut o = OpusOptions::new(16_000);
    o.application = Application::Voip;
    let (file, stats) = encode_opus_ogg_with(&sp, 1, 48_000, &o).unwrap();
    check_structure(&file, 1, 48_000, frames);
    assert_eq!(stats.frames_out, frames as u64);
    let dec = decode(&file);
    let d = lufs(&dec, 48_000) - lufs(&[sp], 48_000);
    assert!(d.abs() < 1.5, "speech at 16 kb/s moved by {d:.2} LU");
}

#[test]
fn lengths_around_a_packet_boundary_are_exact() {
    // 1 sample, a packet less/exactly/more, and lengths around the pre-skip + packet boundary
    for frames in [1usize, 2, 119, 120, 121, 839, 840, 841, 959, 960, 961, 1799, 1920, 48_000 + 1] {
        let input = test_signal(48_000, frames, 2);
        let file = encode_opus_ogg(&interleave(&input), 2, 48_000, 96_000).unwrap();
        check_structure(&file, 2, 48_000, frames);
        let dec = decode(&file);
        assert_eq!(dec[0].len(), frames, "{frames} frames");
    }
    for frames in [1usize, 2, 3, 50, 441, 44_099, 44_101] {
        let input = test_signal(44_100, frames, 1);
        let file = encode_opus_ogg(&interleave(&input), 1, 44_100, 64_000).unwrap();
        check_structure(&file, 1, 44_100, frames);
        assert_eq!(decode(&file)[0].len(), expected_frames(frames, 44_100), "{frames} frames at 44.1 kHz");
    }
}

#[test]
fn silence_is_a_valid_tiny_file() {
    let file = encode_opus_ogg(&vec![0.0; 2 * 48_000], 2, 48_000, 128_000).unwrap();
    check_structure(&file, 2, 48_000, 48_000);
    assert!(file.len() < 4_000, "silence took {} bytes", file.len());
    assert!(decode(&file).iter().all(|c| c.iter().all(|v| v.abs() < 1e-6)));
}

// ---- streaming and metadata ---------------------------------------------------------------------------

#[test]
fn streaming_in_odd_chunks_matches_one_shot() {
    for rate in [48_000u32, 44_100] {
        let input = interleave(&test_signal(rate, 30_000, 2));
        let whole = encode_opus_ogg(&input, 2, rate, 96_000).unwrap();
        for chunk in [2usize, 6, 1_922, 9_999 * 2, 100_000] {
            let mut w = OpusOggWriter::new(Vec::new(), 2, rate, &OpusOptions::new(96_000)).unwrap();
            for c in input.chunks(chunk) {
                w.write_interleaved(c).unwrap();
            }
            let (out, stats) = w.finish().unwrap();
            assert_eq!(out, whole, "{rate} Hz, chunks of {chunk}");
            assert_eq!(stats.frames_in, 30_000);
        }
    }
}

#[test]
fn tags_carry_the_vendor_and_comments() {
    let mut o = OpusOptions::new(64_000);
    o.vendor = "Test Tool 1".into();
    o.comments = vec![("TITLE".into(), "Bounce 1".into()), ("ARTIST".into(), "ü".into())];
    let (file, _) = encode_opus_ogg_with(&vec![0.1; 4_800], 1, 48_000, &o).unwrap();
    let tags = parse(&file).tags;
    let text = String::from_utf8_lossy(&tags);
    assert!(text.contains("Test Tool 1") && text.contains("TITLE=Bounce 1") && text.contains("ARTIST=ü"), "{text}");
    // an invalid key is refused up front
    o.comments = vec![("A=B".into(), "x".into())];
    assert!(encode_opus_ogg_with(&[0.1; 480], 1, 48_000, &o).is_err());
}

#[test]
fn the_output_is_deterministic() {
    let input = interleave(&test_signal(44_100, 20_000, 2));
    assert_eq!(encode_opus_ogg(&input, 2, 44_100, 96_000).unwrap(), encode_opus_ogg(&input, 2, 44_100, 96_000).unwrap());
}

#[test]
fn long_files_use_many_pages() {
    let input = interleave(&test_signal(48_000, 48_000 * 12, 2));
    let file = encode_opus_ogg(&input, 2, 48_000, 192_000).unwrap();
    check_structure(&file, 2, 48_000, 48_000 * 12);
    assert!(pages(&file).len() >= 2 + 12, "one page per second of packets");
}

// ---- hostile input ---------------------------------------------------------------------------------------

#[test]
fn bad_parameters_are_errors() {
    let ok = vec![0.1f32; 4800];
    assert_eq!(encode_opus_ogg(&[], 2, 48_000, 96_000), Err(Error::Empty));
    assert_eq!(encode_opus_ogg(&[], 1, 48_000, 96_000), Err(Error::Empty));
    assert_eq!(encode_opus_ogg(&ok, 0, 48_000, 96_000), Err(Error::InvalidChannels(0)));
    for ch in [3u16, 5, 6, 7, 8, 9, 255, 256, 257, u16::MAX] {
        assert_eq!(encode_opus_ogg(&ok, ch, 48_000, 96_000), Err(Error::InvalidChannels(ch)), "{ch} channels");
    }
    for rate in [0u32, 1, 100, 7_999, 192_001, 384_000, 1_000_000, u32::MAX - 1, u32::MAX] {
        assert_eq!(encode_opus_ogg(&ok, 2, rate, 96_000), Err(Error::InvalidSampleRate(rate)), "{rate} Hz");
    }
    for br in [0u32, 1, 5_999, 11_999, 510_001, 1_000_000, u32::MAX] {
        assert!(matches!(encode_opus_ogg(&ok, 2, 48_000, br), Err(Error::InvalidBitrate { .. })), "{br} b/s");
    }
    assert_eq!(encode_opus_ogg(&ok[..4799], 2, 48_000, 96_000), Err(Error::Misaligned { len: 4799, channels: 2 }));
    assert!(encode_opus_ogg(&ok[..1], 2, 48_000, 96_000).is_err());
    // the edges of the valid range work
    for (ch, rate, br) in [(1u16, 8_000u32, 6_000u32), (2, 192_000, MAX_BITRATE), (1, 48_000, MAX_BITRATE), (2, 8_000, 12_000)] {
        assert!(encode_opus_ogg(&vec![0.1; 4800 * usize::from(ch)], ch, rate, br).is_ok(), "{ch} ch {rate} Hz {br} b/s");
    }
}

#[test]
fn a_finished_or_empty_writer_is_an_error_not_a_panic() {
    let w = OpusOggWriter::new(Vec::new(), 2, 48_000, &OpusOptions::new(96_000)).unwrap();
    assert_eq!(w.finish().err(), Some(Error::Empty));
    let mut w = OpusOggWriter::new(Vec::new(), 2, 48_000, &OpusOptions::new(96_000)).unwrap();
    assert!(w.write_interleaved(&[0.0; 3]).is_err());
    w.write_interleaved(&[]).unwrap();
    assert_eq!(w.finish().err(), Some(Error::Empty));
}

/// A sink that refuses writes.
struct Broken;
impl std::io::Write for Broken {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("disk full"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failing_sink_is_an_error() {
    assert!(matches!(OpusOggWriter::new(Broken, 2, 48_000, &OpusOptions::new(96_000)), Err(Error::Io(_))));
}

#[test]
fn non_finite_and_huge_samples_are_sanitised() {
    let mut x = interleave(&test_signal(48_000, 48_000, 2));
    for (i, v) in x.iter_mut().enumerate().step_by(97) {
        *v = match i % 7 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => f32::NEG_INFINITY,
            3 => 1.0e30,
            4 => -3.0,
            5 => f32::MAX,
            _ => f32::MIN_POSITIVE / 8.0,
        };
    }
    let (file, stats) = encode_opus_ogg_with(&x, 2, 48_000, &OpusOptions::new(128_000)).unwrap();
    assert!(stats.non_finite > 0 && stats.clipped > 0, "{stats:?}");
    check_structure(&file, 2, 48_000, 48_000);
    let dec = decode(&file);
    assert!(dec.iter().flatten().all(|v| v.is_finite()));
    assert!(dec.iter().flatten().all(|v| v.abs() < 2.0));
}

/// The frame-overflow counter (the donor's debug assertion, kept as a count) stays 0 on ordinary music-like
/// input at every rate, channel count and application, through both the one-shot and the streaming API.
#[test]
fn the_frame_overflow_counter_stays_zero_on_normal_input() {
    for (rate, ch) in [(48_000u32, 2usize), (48_000, 1), (44_100, 2), (16_000, 1), (96_000, 2)] {
        let x = interleave(&test_signal(rate, rate as usize, ch));
        for bitrate in [12_000u32 * ch as u32, 64_000, 128_000, 320_000] {
            for app in [Application::Audio, Application::Voip] {
                let mut o = OpusOptions::new(bitrate);
                o.application = app;
                let (file, stats) = encode_opus_ogg_with(&x, ch as u16, rate, &o).unwrap();
                assert!(stats.packets > 0);
                assert_eq!(stats.frame_overflows, 0, "{rate} Hz {ch} ch {bitrate} b/s {app:?}: {stats:?}");
                check_structure(&file, ch, rate, rate as usize);

                let mut w = OpusOggWriter::new(Vec::new(), ch as u16, rate, &o).unwrap();
                for chunk in x.chunks(977 * ch) {
                    w.write_interleaved(chunk).unwrap();
                    assert_eq!(w.stats().frame_overflows, 0);
                }
                let (_, streamed) = w.finish().unwrap();
                assert_eq!(streamed.frame_overflows, 0, "{rate} Hz {ch} ch {bitrate} b/s {app:?} streamed");
            }
        }
    }
}

/// Signals that stress the quantisers: every mode and bitrate must produce a decodable file. The
/// test profile has overflow checks and debug assertions, so a bad shift or index shows up as a panic.
#[test]
fn stress_signals_encode_and_decode_in_every_mode() {
    let frames = 9_600usize;
    let mut r = Rng(42);
    let signals: Vec<(&str, Vec<f32>)> = vec![
        ("white noise, full scale", (0..frames).map(|_| r.next() as f32).collect()),
        ("square wave", (0..frames).map(|n| if (n / 24) % 2 == 0 { 1.0 } else { -1.0 }).collect()),
        ("nyquist", (0..frames).map(|n| if n % 2 == 0 { 1.0 } else { -1.0 }).collect()),
        ("impulses", (0..frames).map(|n| if n % 1000 == 0 { 1.0 } else { 0.0 }).collect()),
        ("dc +1", vec![1.0; frames]),
        ("dc -1", vec![-1.0; frames]),
        ("silence", vec![0.0; frames]),
        ("denormals", vec![f32::MIN_POSITIVE / 1024.0; frames]),
        ("tiny noise", (0..frames).map(|_| r.next() as f32 * 1e-7).collect()),
        ("sweep", (0..frames).map(|n| (std::f64::consts::PI * (n as f64).powi(2) / frames as f64 * 0.5).sin() as f32).collect()),
        ("bursts", (0..frames).map(|n| if (n / 1500) % 2 == 0 { r.next() as f32 } else { 0.0 }).collect()),
    ];
    for (name, mono) in &signals {
        for (ch, bitrate) in [
            (1u16, 6_000u32),
            (1, 8_000),
            (1, 12_000),
            (1, 16_000),
            (1, 24_000),
            (1, 32_000),
            (1, 64_000),
            (1, 510_000),
            (2, 12_000),
            (2, 24_000),
            (2, 40_000),
            (2, 96_000),
            (2, 510_000),
        ] {
            let input: Vec<f32> = if ch == 1 { mono.clone() } else { mono.iter().flat_map(|v| [*v, -*v * 0.5]).collect() };
            for app in [Application::Audio, Application::Voip] {
                eprintln!("stress: {name}, {ch} ch, {bitrate} b/s, {app:?}");
                let mut o = OpusOptions::new(bitrate);
                o.application = app;
                let (file, stats) = encode_opus_ogg_with(&input, ch, 48_000, &o).unwrap_or_else(|e| panic!("{name} {ch} ch {bitrate}: {e}"));
                assert_eq!(stats.frame_overflows, 0, "{name} {ch} ch {bitrate} b/s {app:?}: {stats:?}");
                let dec = decode(&file);
                assert_eq!(dec.len(), usize::from(ch), "{name}");
                assert_eq!(dec[0].len(), frames, "{name} {ch} ch {bitrate} b/s {app:?}");
                assert!(dec.iter().flatten().all(|v| v.is_finite()), "{name} {ch} ch {bitrate}");
            }
        }
    }
}

/// Random signals (noise, tones, bursts, clipping, dropouts) at random levels, bitrates and
/// applications: every file must be well formed and decode to the right length. The test profile has
/// overflow checks and debug assertions on, so an out-of-range shift or index panics here.
/// `SOUNDCRAFT_FUZZ_CASES` raises the number of cases (default 120).
#[test]
fn random_signals_at_random_bitrates_decode() {
    let cases: usize = std::env::var("SOUNDCRAFT_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
    let mut r = Rng(0xC0FF_EE11);
    let mut pick = |n: usize| ((r.next() * 0.5 + 0.5) * n as f64) as usize % n;
    for case in 0..cases {
        let ch = 1 + pick(2) as u16;
        let rate = [8_000u32, 16_000, 24_000, 44_100, 48_000, 48_000, 48_000, 96_000][pick(8)];
        let frames = 2_000 + pick(20_000);
        let bitrate = 6_000 * u32::from(ch) + pick(60) as u32 * pick(8_500) as u32 % (MAX_BITRATE - 6_000 * u32::from(ch));
        let level = [0.0f64, 1e-9, 1e-4, 0.05, 0.5, 1.0, 4.0][pick(7)];
        let kind = pick(5);
        let mut g = Rng(1 + case as u64 * 7919);
        let hz = 50.0 + g.next().abs() * 9_000.0;
        let x: Vec<f32> = (0..frames * usize::from(ch))
            .map(|i| {
                let n = i / usize::from(ch);
                let t = n as f64 / f64::from(rate);
                let v = match kind {
                    0 => g.next(),
                    1 => (2.0 * std::f64::consts::PI * hz * t).sin(),
                    2 => {
                        if (n / 700) % 3 == 0 {
                            g.next()
                        } else {
                            0.0
                        }
                    }
                    3 => ((2.0 * std::f64::consts::PI * hz * t).sin() * 3.0).clamp(-1.0, 1.0) + 0.3 * g.next(),
                    _ => {
                        if n % 311 < 5 {
                            g.next() * 4.0
                        } else {
                            0.01 * g.next()
                        }
                    }
                };
                (v * level) as f32
            })
            .collect();
        let mut o = OpusOptions::new(bitrate);
        o.application = if pick(2) == 0 { Application::Audio } else { Application::Voip };
        let (file, stats) = encode_opus_ogg_with(&x, ch, rate, &o).unwrap_or_else(|e| panic!("case {case}: {e}"));
        assert_eq!(stats.frame_overflows, 0, "case {case}: {ch} ch {rate} Hz {bitrate} b/s kind {kind} level {level}: {stats:?}");
        check_structure(&file, usize::from(ch), rate, frames);
        let dec = decode(&file);
        assert_eq!(dec[0].len(), expected_frames(frames, rate), "case {case}: {ch} ch {rate} Hz {bitrate} b/s kind {kind} level {level}");
        assert!(dec.iter().flatten().all(|v| v.is_finite()), "case {case}");
    }
}

mod fuzz {
    use super::*;
    use proptest::prelude::*;

    fn sample() -> impl Strategy<Value = f32> {
        prop_oneof![
            6 => -1.0f32..1.0,
            1 => Just(f32::NAN),
            1 => Just(f32::INFINITY),
            1 => Just(f32::NEG_INFINITY),
            1 => Just(f32::MAX),
            1 => Just(f32::MIN),
            1 => Just(0.0f32),
            1 => any::<f32>(),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(40))]

        /// Any valid parameters and any samples give a well-formed file of the right length.
        #[test]
        fn valid_input_always_gives_a_playable_file(
            ch in 1u16..=2,
            rate in 8_000u32..=192_000,
            extra_bitrate in 0u32..=498_000,
            frames in 1usize..3_000,
            samples in proptest::collection::vec(sample(), 1..400),
        ) {
            let bitrate = 6_000 * u32::from(ch) + extra_bitrate;
            let input: Vec<f32> = (0..frames * usize::from(ch)).map(|i| samples[i % samples.len()]).collect();
            let (file, stats) = encode_opus_ogg_with(&input, ch, rate, &OpusOptions::new(bitrate)).unwrap();
            prop_assert_eq!(stats.frame_overflows, 0);
            check_structure(&file, usize::from(ch), rate, frames);
            let dec = decode(&file);
            prop_assert_eq!(dec[0].len(), expected_frames(frames, rate));
            prop_assert!(dec.iter().flatten().all(|v| v.is_finite()));
        }

        /// Any parameters at all give a result, never a panic.
        #[test]
        fn arbitrary_parameters_never_panic(
            ch in any::<u16>(),
            rate in any::<u32>(),
            bitrate in any::<u32>(),
            samples in proptest::collection::vec(any::<f32>(), 0..300),
        ) {
            let r = encode_opus_ogg(&samples, ch, rate, bitrate);
            let valid = (1..=2).contains(&ch) && (8_000..=192_000).contains(&rate) && (6_000 * u32::from(ch)..=510_000).contains(&bitrate);
            if valid && !samples.is_empty() && samples.len() % usize::from(ch) == 0 {
                prop_assert!(r.is_ok());
            } else {
                prop_assert!(r.is_err());
            }
        }
    }
}

// ---- ffmpeg as an outside check (test tool only) ------------------------------------------------------------

fn tool(name: &str) -> Option<String> {
    std::process::Command::new(name).arg("-version").output().ok().filter(|o| o.status.success()).map(|_| name.to_string())
}

#[test]
#[ignore = "needs ffmpeg and ffprobe installed (a test tool, never a dependency)"]
fn ffmpeg_reads_our_files_without_errors() {
    let (Some(ffmpeg), Some(ffprobe)) = (tool("ffmpeg"), tool("ffprobe")) else {
        panic!("ffmpeg/ffprobe not installed");
    };
    let dir = std::env::temp_dir().join(format!("soundcraft-opus-ffmpeg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (ch, rate, br) in
        [(1u16, 48_000u32, 64_000u32), (2, 48_000, 128_000), (2, 44_100, 192_000), (1, 16_000, 24_000), (2, 96_000, 256_000), (1, 8_000, 12_000)]
    {
        let frames = rate as usize * 3 + 123;
        let input = test_signal(rate, frames, usize::from(ch));
        let file = encode_opus_ogg(&interleave(&input), ch, rate, br).unwrap();
        let path = dir.join(format!("t-{ch}-{rate}-{br}.opus"));
        std::fs::write(&path, &file).unwrap();
        for decoder in ["opus", "libopus"] {
            let out = std::process::Command::new(&ffmpeg)
                .args(["-v", "error", "-c:a", decoder, "-i"])
                .arg(&path)
                .args(["-f", "null", "-"])
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            if err.contains("Unknown decoder") {
                eprintln!("ffmpeg has no {decoder} decoder");
                continue;
            }
            assert!(out.status.success() && err.trim().is_empty(), "{decoder} {ch} ch {rate} Hz: {err}");
        }
        let probe = std::process::Command::new(&ffprobe)
            .args(["-v", "error", "-show_entries", "stream=codec_name,channels:format=duration", "-of", "default=nw=1"])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&probe.stdout).to_string();
        assert!(probe.status.success() && probe.stderr.is_empty(), "ffprobe: {}", String::from_utf8_lossy(&probe.stderr));
        let duration: f64 = text.lines().find_map(|l| l.strip_prefix("duration=")).unwrap().parse().unwrap();
        // ffprobe's container duration is the last granule position, which counts the pre-skip; the
        // length of the decoded audio is checked separately below
        let want = (expected_frames(frames, rate) as u64 + PRE_SKIP) as f64 / 48_000.0;
        eprintln!("{ch} ch {rate} Hz {br} b/s: ffprobe duration {duration:.6} s, expected {want:.6} s (last granule)");
        assert!((duration - want).abs() < 0.0005, "duration {duration} vs {want}");
        assert!(text.contains("codec_name=opus") && text.contains(&format!("channels={ch}")), "{text}");
        // decoded loudness through ffmpeg as well
        let pcm = std::process::Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "f32le", "-ac", &ch.to_string(), "-ar", "48000", "-"])
            .output()
            .unwrap();
        let (words, _) = pcm.stdout.as_chunks::<4>();
        let samples: Vec<f32> = words.iter().map(|b| f32::from_le_bytes(*b)).collect();
        assert_eq!(samples.len(), expected_frames(frames, rate) * usize::from(ch), "ffmpeg decoded length");
        let planar: Vec<Vec<f32>> = (0..usize::from(ch)).map(|c| samples.iter().skip(c).step_by(usize::from(ch)).copied().collect()).collect();
        let delta = lufs(&planar, 48_000) - lufs(&input, rate);
        eprintln!("   ffmpeg-decoded loudness delta {delta:+.3} LU");
        assert!(delta.abs() < 1.0);
        let _ = rms(&planar[0]);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
