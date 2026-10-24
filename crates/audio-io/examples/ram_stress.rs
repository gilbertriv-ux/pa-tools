// RAM stress test for streamed playback (G-06).
// Drop into crates/audio-io/examples/ram_stress.rs AFTER Stage 2 lands, then:
//   cargo run --release -p soundcraft-audio-io --example ram_stress
// Proves: reading a ~1h file via the Streamed backing keeps RSS flat (MBs), not file-size (GBs).
//
// API used (confirmed from Stage 1 on disk):
//   StreamReader::open(path) -> Result<StreamReader>
//   StreamReader::{frames()->u64, channels()->u16, sample_rate()->u32, read_frames(ch,start,out)->usize, build_peaks()->Result<Vec<Peaks>>}
// Note: this harness exercises audio-io's StreamReader directly (the deinterleaving read path),
// which is what the mix pre-read worker calls. It does NOT need soundcraft-model, so it stays a
// pure audio-io example. Confirm FileFormat variant name at drop-in if SourceAudio is used instead.

use soundcraft_audio_io::StreamReader;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Instant;

const RATE: u32 = 48_000;
const SECS: u64 = 3600; // 1 hour
const CH: u16 = 2;
const BLOCK: usize = 512;

fn rss_kb() -> u64 {
    // VmRSS = current resident set size (what's actually in RAM right now).
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
        }
    }
    0
}
fn hwm_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
        }
    }
    0
}

fn write_big_wav(path: &str) -> std::io::Result<u64> {
    // 16-bit PCM stereo, written incrementally so generation itself never holds the file in RAM.
    let frames = RATE as u64 * SECS;
    let data_bytes = frames * CH as u64 * 2;
    let riff = 36 + data_bytes;
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(b"RIFF")?;
    w.write_all(&(riff as u32).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&CH.to_le_bytes())?;
    w.write_all(&RATE.to_le_bytes())?;
    let byte_rate = RATE * CH as u32 * 2;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&(CH * 2).to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?; // bits
    w.write_all(b"data")?;
    w.write_all(&(data_bytes as u32).to_le_bytes())?;
    // simple sine so data isn't all zero
    let mut buf = Vec::with_capacity(BLOCK * CH as usize * 2);
    let mut n = 0u64;
    while n < frames {
        buf.clear();
        let take = BLOCK.min((frames - n) as usize);
        for i in 0..take {
            let t = (n + i as u64) as f64 / RATE as f64;
            let s = ((t * 440.0 * std::f64::consts::TAU).sin() * 12000.0) as i16;
            for _ in 0..CH {
                buf.extend_from_slice(&s.to_le_bytes());
            }
        }
        w.write_all(&buf)?;
        n += take as u64;
    }
    w.flush()?;
    Ok(data_bytes + 44)
}

fn main() {
    let path = "/tmp/ram_stress_1h.wav";
    eprintln!("generating 1h stereo 48k WAV at {path} ...");
    let size = write_big_wav(path).expect("write wav");
    eprintln!("file size: {:.1} MB", size as f64 / 1e6);

    let base = rss_kb();
    let reader = StreamReader::open(path).expect("open stream");
    let frames = reader.frames();
    eprintln!("opened: {} frames, {} ch, {} Hz; RSS after open: {} MB", frames, reader.channels(), reader.sample_rate(), rss_kb() / 1024);

    // Background-style peaks pass (streams the whole file once).
    let t0 = Instant::now();
    let _peaks = reader.build_peaks().expect("peaks");
    eprintln!("peaks built in {:?}; RSS after peaks: {} MB", t0.elapsed(), rss_kb() / 1024);

    // Simulate the audio read path: pull every 512-frame block across the whole hour.
    let mut scratch = vec![0.0f32; BLOCK];
    let mut peak_rss = rss_kb();
    let mut underruns = 0u64;
    let mut start = 0u64;
    let loops = frames / BLOCK as u64;
    let t1 = Instant::now();
    while start < frames {
        for ch in 0..reader.channels() as usize {
            let got = reader.read_frames(ch, start, &mut scratch);
            if got < scratch.len().min((frames - start) as usize) {
                underruns += 1;
            }
        }
        start += BLOCK as u64;
        if start % (RATE as u64 * 60) < BLOCK as u64 {
            peak_rss = peak_rss.max(rss_kb());
        }
    }
    peak_rss = peak_rss.max(rss_kb());
    let dur = t1.elapsed();

    eprintln!("--- RESULT ---");
    eprintln!("file on disk:        {:.1} MB", size as f64 / 1e6);
    eprintln!("RSS before open:     {} MB", base / 1024);
    eprintln!("peak RSS during read:{} MB", peak_rss / 1024);
    eprintln!("VmHWM (lifetime):    {} MB", hwm_kb() / 1024);
    eprintln!("blocks read:         {} ({} underruns)", loops * CH as u64, underruns);
    eprintln!("read {} frames x{}ch in {:?}", frames, CH, dur);
    let flat = peak_rss / 1024 < 200; // < 200 MB == streaming (file is ~700MB)
    eprintln!("VERDICT: {}", if flat && underruns == 0 { "PASS (RSS flat, no underruns)" } else { "CHECK (RSS high or underruns)" });
}
