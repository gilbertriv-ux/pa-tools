//! Streamed reads of uncompressed PCM files (WAV, AIFF/AIFF-C, CAF).
//!
//! A [`StreamReader`] parses only a file's header (reusing the chunk parsers in
//! [`crate::wav`] / [`crate::aiff`] / [`crate::caf`]) to learn where the sample data is, how it is
//! laid out and how it is stored, then reads just the bytes a request needs — it never decodes or
//! holds the whole file. The crate stays allocation-bounded and (deliberately) does no memory
//! mapping: reads go through a seek + bounded read on a `Read + Seek` source.
//!
//! The reader is generic over the source so it builds everywhere (including `wasm32`, which has no
//! usable filesystem): [`StreamReader::from_source`] takes any `Read + Seek + Send` (a file, a
//! `Cursor<Vec<u8>>`, ...), while the convenience [`StreamReader::open`] (native targets only)
//! opens a file by path.

use crate::pcm::{PcmKind, decode_sample};
use crate::peaks::{Peaks, PeaksBuilder};
use crate::{AudioError, FileFormat, Result, SampleFormat, aiff, caf, detect_format, wav};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

/// The largest header prefix we read to find the data chunk. Headers of uncompressed files are
/// tiny (a `fmt `/`COMM`/`desc` chunk, maybe a `bext`); this cap keeps a hostile file from making
/// us read an unbounded amount before we find — or give up on — the audio.
const HEADER_CAP: usize = 1 << 20;

/// The most bytes a single read pulls from the source, so one `read_frames` call never allocates
/// or reads an unbounded amount regardless of how many frames the caller asks for.
const MAX_READ_BYTES: usize = 1 << 16;

/// Where the sample data of an uncompressed file is and how it is stored. Produced by the native
/// header parsers; `data_len` is already clamped to what the file actually holds.
pub(crate) struct NativeLayout {
    pub sample_format: SampleFormat,
    pub sample_rate: u32,
    pub channels: u16,
    /// Byte offset of the first sample frame.
    pub data_start: u64,
    /// Bytes of sample data (clamped to the real file length).
    pub data_len: u64,
    pub kind: PcmKind,
}

/// A `Read + Seek` source the reader owns. The blanket impl covers files and in-memory cursors.
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

struct Inner {
    source: Box<dyn ReadSeek>,
    /// Reused between reads so steady-state reading allocates nothing.
    scratch: Vec<u8>,
}

/// Reads sample frames of one uncompressed PCM file on demand, converting to `f32`.
pub struct StreamReader {
    inner: Mutex<Inner>,
    layout: NativeLayout,
    /// Total frames of real audio (data length / frame size).
    frames: u64,
}

impl std::fmt::Debug for StreamReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamReader")
            .field("sample_format", &self.layout.sample_format)
            .field("sample_rate", &self.layout.sample_rate)
            .field("channels", &self.layout.channels)
            .field("frames", &self.frames)
            .field("data_start", &self.layout.data_start)
            .finish()
    }
}

/// Read into `buf` until it is full or the source ends; returns the bytes actually read. A short
/// return means end of file (a truncated recording), not an error — those propagate.
fn read_filling(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(buf.get_mut(filled..).unwrap_or(&mut [])) {
            Ok(0) => break,
            Ok(n) => filled = filled.saturating_add(n),
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

impl StreamReader {
    /// Open an uncompressed PCM file from any seekable source. `file_len` is the source's total
    /// length in bytes (used to size the sample data without scanning the whole file); `ext_hint`
    /// is an optional extension (`"wav"`, `".aiff"`, ...) used only when the magic bytes are
    /// ambiguous. Returns [`AudioError::Unsupported`] for a compressed file.
    pub fn from_source<R: ReadSeek + 'static>(mut source: R, file_len: u64, ext_hint: Option<&str>) -> Result<Self> {
        source.seek(SeekFrom::Start(0))?;
        let cap = usize::try_from(file_len).unwrap_or(usize::MAX).min(HEADER_CAP);
        let mut head = Vec::new();
        head.try_reserve_exact(cap).map_err(|_| AudioError::TooLarge(format!("cannot buffer {cap} header bytes")))?;
        head.resize(cap, 0);
        let got = read_filling(&mut source, &mut head)?;
        head.truncate(got);

        let format = detect_format(&head, ext_hint);
        let layout = match format {
            FileFormat::Wav => wav::stream_layout(&head, file_len)?,
            FileFormat::Aiff => aiff::stream_layout(&head, file_len)?,
            FileFormat::Caf => caf::stream_layout(&head, file_len)?,
            other => return Err(AudioError::Unsupported(format!("{other:?} cannot be streamed (only uncompressed WAV, AIFF and CAF)"))),
        };
        let layout = layout.ok_or_else(|| AudioError::Unsupported(format!("{format:?} uses a codec that cannot be streamed")))?;

        let block = layout.kind.bytes().checked_mul(usize::from(layout.channels)).filter(|&b| b > 0);
        let block = block.ok_or_else(|| AudioError::Malformed("zero-sized sample frame".into()))?;
        let frames = layout.data_len / block as u64;
        Ok(StreamReader { inner: Mutex::new(Inner { source: Box::new(source), scratch: Vec::new() }), layout, frames })
    }

    /// Open an uncompressed PCM file by path (native targets only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)?;
        let file_len = file.metadata()?.len();
        let ext = path.extension().and_then(|e| e.to_str());
        Self::from_source(file, file_len, ext)
    }

    /// Total frames of audio.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Channel count.
    pub fn channels(&self) -> u16 {
        self.layout.channels
    }

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.layout.sample_rate
    }

    /// How samples are stored in the file.
    pub fn sample_format(&self) -> SampleFormat {
        self.layout.sample_format
    }

    /// Frame size in bytes, or `None` on overflow.
    fn frame_bytes(&self) -> Option<usize> {
        self.layout.kind.bytes().checked_mul(usize::from(self.layout.channels)).filter(|&b| b > 0)
    }

    /// Read up to `out.len()` frames of channel `ch` starting at frame `start`, converting to
    /// `f32`. Out-of-range channels, reads past the end, and truncated files all produce silence
    /// in the untouched tail of `out`. Returns the number of real frames written (0 .. out.len());
    /// the rest of `out` is zeroed. Never blocks on anything but the source read, never allocates
    /// unboundedly, never panics.
    pub fn read_frames(&self, ch: usize, start: u64, out: &mut [f32]) -> usize {
        for v in out.iter_mut() {
            *v = 0.0;
        }
        let channels = usize::from(self.layout.channels);
        if ch >= channels || out.is_empty() || start >= self.frames {
            return 0;
        }
        let sample_bytes = self.layout.kind.bytes();
        let Some(block) = self.frame_bytes() else { return 0 };
        let want = (out.len() as u64).min(self.frames - start);
        let want = usize::try_from(want).unwrap_or(usize::MAX);
        let Some(start_byte) = start.checked_mul(block as u64).and_then(|o| o.checked_add(self.layout.data_start)) else {
            return 0;
        };

        let frames_per_read = (MAX_READ_BYTES / block).max(1);
        let Ok(mut guard) = self.inner.lock() else { return 0 };
        let Inner { source, scratch } = &mut *guard;
        if source.seek(SeekFrom::Start(start_byte)).is_err() {
            return 0;
        }
        let want_bytes = frames_per_read.saturating_mul(block);
        let need = want_bytes.saturating_sub(scratch.len());
        if need > 0 {
            if scratch.try_reserve(need).is_err() {
                return 0;
            }
            scratch.resize(want_bytes, 0);
        }

        let kind = self.layout.kind;
        let off = ch.saturating_mul(sample_bytes);
        let mut done = 0usize;
        while done < want {
            let n = (want - done).min(frames_per_read);
            let bytes = n.saturating_mul(block);
            let Some(buf) = scratch.get_mut(..bytes) else { break };
            let filled = match read_filling(source.as_mut(), buf) {
                Ok(f) => f,
                Err(_) => break,
            };
            let full = filled / block;
            for f in 0..full {
                let base = f.saturating_mul(block).saturating_add(off);
                let sample = scratch.get(base..base.saturating_add(sample_bytes)).unwrap_or_default();
                if let Some(dst) = out.get_mut(done + f) {
                    *dst = decode_sample(sample, kind);
                }
            }
            done += full;
            if full < n {
                break; // short read: end of a truncated file
            }
        }
        done
    }

    /// Stream the whole file once to build a waveform overview per channel, without ever holding
    /// the whole file in memory. A truncated file yields overviews of whatever audio is present.
    pub fn build_peaks(&self) -> Result<Vec<Peaks>> {
        let channels = usize::from(self.layout.channels);
        let mut builders: Vec<PeaksBuilder> = (0..channels).map(|_| PeaksBuilder::new()).collect();
        if self.frames == 0 || channels == 0 {
            return Ok(builders.into_iter().map(PeaksBuilder::finish).collect());
        }
        let sample_bytes = self.layout.kind.bytes();
        let Some(block) = self.frame_bytes() else {
            return Err(AudioError::Malformed("zero-sized sample frame".into()));
        };
        let kind = self.layout.kind;
        let frames_per_read = (MAX_READ_BYTES / block).max(1);
        let want_bytes = frames_per_read.saturating_mul(block);

        let mut guard = self.inner.lock().map_err(|_| AudioError::Io(std::io::Error::other("stream reader lock poisoned")))?;
        let Inner { source, scratch } = &mut *guard;
        source.seek(SeekFrom::Start(self.layout.data_start))?;
        let need = want_bytes.saturating_sub(scratch.len());
        if need > 0 {
            scratch.try_reserve(need).map_err(|_| AudioError::TooLarge("cannot allocate stream scratch".into()))?;
            scratch.resize(want_bytes, 0);
        }

        let mut remaining = self.frames;
        while remaining > 0 {
            let n = usize::try_from(remaining.min(frames_per_read as u64)).unwrap_or(frames_per_read);
            let bytes = n.saturating_mul(block);
            let Some(buf) = scratch.get_mut(..bytes) else { break };
            let filled = read_filling(source.as_mut(), buf)?;
            let full = filled / block;
            for f in 0..full {
                for (ch, builder) in builders.iter_mut().enumerate() {
                    let base = f.saturating_mul(block).saturating_add(ch.saturating_mul(sample_bytes));
                    let sample = scratch.get(base..base.saturating_add(sample_bytes)).unwrap_or_default();
                    builder.push(decode_sample(sample, kind));
                }
            }
            remaining = remaining.saturating_sub(full as u64);
            if full < n {
                break; // truncated file
            }
        }
        Ok(builders.into_iter().map(PeaksBuilder::finish).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peaks::Peaks;
    use crate::{AudioBuffer, BitDepth, EncodeOptions};
    use std::io::Cursor;

    fn buffer(channels: &[&[f32]], sr: u32) -> AudioBuffer {
        AudioBuffer { sample_rate: sr, channels: channels.iter().map(|c| c.to_vec()).collect() }
    }

    fn encode(buf: &AudioBuffer, format: FileFormat, depth: BitDepth) -> Vec<u8> {
        let opts = EncodeOptions { format, bit_depth: depth, dither: false, bwf: None };
        crate::encode(buf, &opts).expect("encode")
    }

    fn reader(bytes: Vec<u8>, ext: &str) -> StreamReader {
        let len = bytes.len() as u64;
        StreamReader::from_source(Cursor::new(bytes), len, Some(ext)).expect("open stream")
    }

    fn read_all(r: &StreamReader, ch: usize) -> Vec<f32> {
        let mut out = vec![-7.0f32; r.frames() as usize];
        let n = r.read_frames(ch, 0, &mut out);
        assert_eq!(n, r.frames() as usize, "read every frame");
        out
    }

    #[test]
    fn wav_float_exact_roundtrip() {
        let left: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let right: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.02).cos() * 0.5).collect();
        let buf = buffer(&[&left, &right], 48_000);
        let r = reader(encode(&buf, FileFormat::Wav, BitDepth::Float32), "wav");
        assert_eq!(r.frames(), 1000);
        assert_eq!(r.channels(), 2);
        assert_eq!(r.sample_rate(), 48_000);
        assert_eq!(r.sample_format(), SampleFormat::Float32);
        assert_eq!(read_all(&r, 0), left);
        assert_eq!(read_all(&r, 1), right);
    }

    #[test]
    fn integer_depths_match_a_full_decode() {
        let left: Vec<f32> = (0..777).map(|i| ((i as f32 * 0.03).sin()) * 0.8).collect();
        let right: Vec<f32> = (0..777).map(|i| ((i as f32 * 0.05).sin()) * -0.6).collect();
        let buf = buffer(&[&left, &right], 44_100);
        for (fmt, ext, depth) in [
            (FileFormat::Wav, "wav", BitDepth::Int16),
            (FileFormat::Wav, "wav", BitDepth::Int24),
            (FileFormat::Wav, "wav", BitDepth::Int32),
            (FileFormat::Aiff, "aiff", BitDepth::Int16),
            (FileFormat::Aiff, "aiff", BitDepth::Int24),
            (FileFormat::Aiff, "aiff", BitDepth::Int32),
            (FileFormat::Aiff, "aiff", BitDepth::Float32),
        ] {
            let bytes = encode(&buf, fmt, depth);
            // Ground truth: the crate's own full decoder.
            let (_info, decoded) = crate::decode(&bytes, Some(ext)).expect("decode");
            let r = reader(bytes, ext);
            assert_eq!(r.frames(), 777, "{fmt:?} {depth:?} frames");
            for ch in 0..2 {
                assert_eq!(read_all(&r, ch), decoded.channels[ch], "{fmt:?} {depth:?} ch{ch}");
            }
        }
    }

    #[test]
    fn seeks_and_partial_windows() {
        let mono: Vec<f32> = (0..5000).map(|i| i as f32 / 5000.0 - 0.5).collect();
        let buf = buffer(&[&mono], 48_000);
        let r = reader(encode(&buf, FileFormat::Wav, BitDepth::Float32), "wav");
        let mut out = vec![0.0f32; 100];
        let n = r.read_frames(0, 2500, &mut out);
        assert_eq!(n, 100);
        assert_eq!(out, mono[2500..2600]);
        // A seek backwards still works.
        let n = r.read_frames(0, 10, &mut out);
        assert_eq!(n, 100);
        assert_eq!(out, mono[10..110]);
    }

    #[test]
    fn zero_pads_past_eof() {
        let mono: Vec<f32> = (0..300).map(|i| (i as f32 * 0.1).sin()).collect();
        let buf = buffer(&[&mono], 48_000);
        let r = reader(encode(&buf, FileFormat::Wav, BitDepth::Float32), "wav");
        let mut out = vec![9.0f32; 50];
        let n = r.read_frames(0, 280, &mut out);
        assert_eq!(n, 20, "only 20 real frames remain");
        assert_eq!(&out[..20], &mono[280..300]);
        assert!(out[20..].iter().all(|&v| v == 0.0), "tail is zero-padded");
        // Entirely past the end.
        let n = r.read_frames(0, 10_000, &mut out);
        assert_eq!(n, 0);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn invalid_channel_is_silence() {
        let mono = [0.1f32, 0.2, 0.3];
        let buf = buffer(&[&mono], 48_000);
        let r = reader(encode(&buf, FileFormat::Wav, BitDepth::Float32), "wav");
        let mut out = vec![1.0f32; 3];
        assert_eq!(r.read_frames(5, 0, &mut out), 0);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn caf_lpcm_roundtrip_via_handwritten_file() {
        // The crate has no CAF encoder, so build a tiny LPCM CAF by hand: 16-bit big-endian stereo.
        let left = [0i16, 100, -200, 32_000, -32_000];
        let right = [1i16, -1, 500, -500, 123];
        let mut data = Vec::new();
        for i in 0..left.len() {
            data.extend_from_slice(&left[i].to_be_bytes());
            data.extend_from_slice(&right[i].to_be_bytes());
        }
        let mut caf = Vec::new();
        caf.extend_from_slice(b"caff");
        caf.extend_from_slice(&1u16.to_be_bytes()); // version
        caf.extend_from_slice(&0u16.to_be_bytes()); // flags
        // desc chunk (size 32).
        caf.extend_from_slice(b"desc");
        caf.extend_from_slice(&32i64.to_be_bytes());
        caf.extend_from_slice(&48_000f64.to_be_bytes());
        caf.extend_from_slice(b"lpcm");
        caf.extend_from_slice(&0u32.to_be_bytes()); // flags: signed int, big-endian
        caf.extend_from_slice(&4u32.to_be_bytes()); // bytes per packet (2ch * 2 bytes)
        caf.extend_from_slice(&1u32.to_be_bytes()); // frames per packet
        caf.extend_from_slice(&2u32.to_be_bytes()); // channels
        caf.extend_from_slice(&16u32.to_be_bytes()); // bits per channel
        // data chunk.
        caf.extend_from_slice(b"data");
        caf.extend_from_slice(&((data.len() + 4) as i64).to_be_bytes());
        caf.extend_from_slice(&0u32.to_be_bytes()); // edit count
        caf.extend_from_slice(&data);

        let r = reader(caf, "caf");
        assert_eq!(r.frames(), 5);
        assert_eq!(r.channels(), 2);
        assert_eq!(r.sample_rate(), 48_000);
        assert_eq!(r.sample_format(), SampleFormat::Int16);
        let got_l = read_all(&r, 0);
        let got_r = read_all(&r, 1);
        for i in 0..5 {
            assert!((got_l[i] - left[i] as f32 / 32_768.0).abs() < 1e-6, "left {i}");
            assert!((got_r[i] - right[i] as f32 / 32_768.0).abs() < 1e-6, "right {i}");
        }
    }

    #[test]
    fn caf_little_endian_float_flags() {
        let samples = [0.25f32, -0.5, 0.75, -1.0];
        let mut data = Vec::new();
        for s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let mut caf = Vec::new();
        caf.extend_from_slice(b"caff");
        caf.extend_from_slice(&1u16.to_be_bytes());
        caf.extend_from_slice(&0u16.to_be_bytes());
        caf.extend_from_slice(b"desc");
        caf.extend_from_slice(&32i64.to_be_bytes());
        caf.extend_from_slice(&44_100f64.to_be_bytes());
        caf.extend_from_slice(b"lpcm");
        caf.extend_from_slice(&0x3u32.to_be_bytes()); // float | little-endian
        caf.extend_from_slice(&4u32.to_be_bytes());
        caf.extend_from_slice(&1u32.to_be_bytes());
        caf.extend_from_slice(&1u32.to_be_bytes());
        caf.extend_from_slice(&32u32.to_be_bytes());
        caf.extend_from_slice(b"data");
        caf.extend_from_slice(&((data.len() + 4) as i64).to_be_bytes());
        caf.extend_from_slice(&0u32.to_be_bytes());
        caf.extend_from_slice(&data);

        let r = reader(caf, "caf");
        assert_eq!(r.sample_format(), SampleFormat::Float32);
        assert_eq!(read_all(&r, 0), samples);
    }

    #[test]
    fn peaks_match_a_full_build() {
        let left: Vec<f32> = (0..9000).map(|i| (i as f32 * 0.013).sin() * 0.9).collect();
        let right: Vec<f32> = (0..9000).map(|i| (i as f32 * 0.017).cos() * 0.4).collect();
        let buf = buffer(&[&left, &right], 48_000);
        let r = reader(encode(&buf, FileFormat::Wav, BitDepth::Float32), "wav");
        let streamed = r.build_peaks().expect("peaks");
        assert_eq!(streamed.len(), 2);
        assert_eq!(streamed[0], Peaks::build(&left));
        assert_eq!(streamed[1], Peaks::build(&right));
    }

    #[test]
    fn truncated_file_reads_partial() {
        let mono: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.02).sin()).collect();
        let buf = buffer(&[&mono], 48_000);
        let mut bytes = encode(&buf, FileFormat::Wav, BitDepth::Float32);
        // Lop off the last 400 bytes (100 float frames).
        bytes.truncate(bytes.len() - 400);
        let len = bytes.len() as u64;
        // Header still declares 1000 frames; the reader must clamp to what is present.
        let r = StreamReader::from_source(Cursor::new(bytes), len, Some("wav")).expect("open");
        assert_eq!(r.frames(), 900, "frames clamped to the real file length");
        let mut out = vec![0.0; 1000];
        let n = r.read_frames(0, 0, &mut out);
        assert_eq!(n, 900);
        assert_eq!(&out[..900], &mono[..900]);
        // Peaks over the truncated file cover only the present audio.
        let p = r.build_peaks().expect("peaks");
        assert_eq!(p[0], Peaks::build(&mono[..900]));
    }

    #[test]
    fn hostile_inputs_never_panic() {
        // Garbage and truncated headers: every one is a clean Err, never a panic.
        for bytes in [vec![], vec![0u8; 3], b"RIFF".to_vec(), b"caff".to_vec(), b"FORMxxxxAIFF".to_vec(), vec![0xFF; 64]] {
            let len = bytes.len() as u64;
            assert!(StreamReader::from_source(Cursor::new(bytes), len, Some("wav")).is_err());
        }
        // A WAV claiming a vast frame count but with almost no data: frames clamp to the file,
        // and reads stay bounded and silent past the end.
        let buf = buffer(&[&[0.5f32, -0.5]], 48_000);
        let bytes = encode(&buf, FileFormat::Wav, BitDepth::Float32);
        let len = bytes.len() as u64;
        let r = StreamReader::from_source(Cursor::new(bytes), len, Some("wav")).expect("open");
        assert_eq!(r.frames(), 2);
        let mut out = vec![0.0; 8];
        assert_eq!(r.read_frames(0, u64::MAX - 1, &mut out), 0);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn open_from_a_real_file() {
        let left: Vec<f32> = (0..400).map(|i| (i as f32 * 0.07).sin()).collect();
        let buf = buffer(&[&left], 48_000);
        let bytes = encode(&buf, FileFormat::Wav, BitDepth::Int24);
        let dir = std::env::temp_dir().join(format!("pa-tools-stream-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("tone.wav");
        std::fs::write(&path, &bytes).expect("write");
        let r = StreamReader::open(&path).expect("open");
        assert_eq!(r.frames(), 400);
        let (_i, decoded) = crate::decode(&bytes, Some("wav")).expect("decode");
        assert_eq!(read_all(&r, 0), decoded.channels[0]);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
