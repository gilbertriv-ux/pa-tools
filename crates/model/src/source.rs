//! Audio sources (files in the session's Audio Files folder) and their decoded data, and the
//! movies video clips reference.

use crate::SourceId;
use soundcraft_audio_io::{AudioBuffer, FileFormat, StreamReader, peaks::Peaks};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Metadata for an audio file used by the session (serialised).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    /// Path relative to the session folder (or absolute for referenced media).
    pub path: String,
    pub channels: u16,
    pub frames: u64,
    pub sample_rate: u32,
    pub format: FileFormat,
    /// Original BWF time stamp (samples), if any.
    #[serde(default)]
    pub time_reference: u64,
    /// True when the file has not been written to disk yet (recorded/rendered this session).
    #[serde(default)]
    pub unsaved: bool,
}

/// A movie used by Video track clips (serialised). The pictures themselves are never stored in
/// the session: the UI opens the movie from `path` and decodes frames on demand.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VideoSource {
    pub id: SourceId,
    pub name: String,
    /// Absolute path of the movie (referenced, never copied into the session folder).
    pub path: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// Frames per second (0 = unknown).
    #[serde(default)]
    pub frame_rate: f64,
    /// Picture length in seconds.
    #[serde(default)]
    pub duration: f64,
    /// Codec id (`h264`, `prores`, `mjpeg`, `hevc`, …).
    #[serde(default)]
    pub codec: String,
    /// Why the movie is not opened, when its stored path was refused on opening the session.
    #[serde(skip)]
    pub offline: Option<String>,
}

impl VideoSource {
    /// Display aspect ratio (width / height), 16:9 when unknown.
    pub fn aspect(&self) -> f64 {
        if self.width == 0 || self.height == 0 { 16.0 / 9.0 } else { f64::from(self.width) / f64::from(self.height) }
    }
}

/// How a [`SourceAudio`]'s samples are stored.
///
/// `Resident` keeps the whole decoded buffer in memory (the only kind before streaming existed and
/// the one the mix engine reads today); `Streamed` reads frames from an uncompressed file on
/// demand through a [`StreamReader`], so a long file never has to fit in RAM. For a `Resident`
/// source the samples live in [`SourceAudio::buffer`]; for a `Streamed` source `buffer` is empty
/// (it still carries the sample rate) and [`SourceAudio::read`] fetches samples through the reader.
#[derive(Debug)]
enum Backing {
    /// Samples live in [`SourceAudio::buffer`].
    Resident,
    /// Samples are read on demand from the file.
    Streamed(Box<Streamed>),
}

/// A streamed, on-demand audio source and its metadata.
#[derive(Debug)]
struct Streamed {
    reader: StreamReader,
    /// Path the file was opened from (relative to the session folder, or absolute for referenced
    /// media), kept so the source can be described and re-opened.
    path: String,
    format: FileFormat,
    frames: u64,
    channels: u16,
    sample_rate: u32,
}

/// Decoded audio (resident) or an on-demand streamed file, plus waveform overviews. Not
/// serialised; shared behind an `Arc` in the [`SourcePool`].
#[derive(Debug)]
pub struct SourceAudio {
    /// Resident samples. Empty for a streamed source (but still carries its sample rate), so the
    /// slice API ([`SourceAudio::channel`]) naturally yields `None` for a streamed source.
    pub buffer: AudioBuffer,
    /// Waveform overviews, one per channel; available for both resident and streamed sources
    /// (built when the source is opened).
    pub peaks: Vec<Peaks>,
    backing: Backing,
}

impl SourceAudio {
    /// A resident source holding the whole decoded buffer in memory (overviews built now).
    pub fn new(buffer: AudioBuffer) -> Self {
        let peaks = buffer.channels.iter().map(|c| Peaks::build(c)).collect();
        SourceAudio { buffer, peaks, backing: Backing::Resident }
    }

    /// A streamed source backed by `reader`, reading frames from `path` on demand. `peaks` are the
    /// overviews built when the file was opened (e.g. via [`StreamReader::build_peaks`]).
    pub fn streamed(reader: StreamReader, path: impl Into<String>, format: FileFormat, peaks: Vec<Peaks>) -> Self {
        let (frames, channels, sample_rate) = (reader.frames(), reader.channels(), reader.sample_rate());
        // A streamed source carries no resident samples, only its sample rate.
        let buffer = AudioBuffer { sample_rate, channels: Vec::new() };
        SourceAudio {
            buffer,
            peaks,
            backing: Backing::Streamed(Box::new(Streamed { reader, path: path.into(), format, frames, channels, sample_rate })),
        }
    }

    /// True for a resident source (the samples are in memory), false for a streamed one.
    pub fn is_resident(&self) -> bool {
        matches!(self.backing, Backing::Resident)
    }

    /// Total number of frames.
    pub fn frames(&self) -> usize {
        match &self.backing {
            Backing::Resident => self.buffer.frames(),
            Backing::Streamed(s) => usize::try_from(s.frames).unwrap_or(usize::MAX),
        }
    }

    /// Number of channels.
    pub fn num_channels(&self) -> usize {
        match &self.backing {
            Backing::Resident => self.buffer.num_channels(),
            Backing::Streamed(s) => usize::from(s.channels),
        }
    }

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        match &self.backing {
            Backing::Resident => self.buffer.sample_rate,
            Backing::Streamed(s) => s.sample_rate,
        }
    }

    /// The whole of channel `ch` as a slice — resident sources only. Returns `None` for a streamed
    /// source (use [`SourceAudio::read`] to pull its frames). This is the pre-streaming "slice API"
    /// the mix engine and UI still use for resident audio.
    pub fn channel(&self, ch: usize) -> Option<&[f32]> {
        match &self.backing {
            Backing::Resident => self.buffer.channels.get(ch).map(Vec::as_slice),
            Backing::Streamed(_) => None,
        }
    }

    /// The whole source as a resident [`AudioBuffer`], reading every frame of a streamed source
    /// into fresh `Vec`s (a plain clone for an already-resident one). Offline/analysis code that
    /// needs whole channels as slices uses this; it allocates the full audio, so callers keep it to
    /// bounded sizes. Never called from the audio callback.
    pub fn load_resident(&self) -> AudioBuffer {
        match &self.backing {
            Backing::Resident => self.buffer.clone(),
            Backing::Streamed(_) => {
                let frames = self.frames();
                let nch = self.num_channels();
                let sr = self.sample_rate();
                // Read each channel in bounded chunks so a huge file never over-allocates scratch;
                // `read` zero-pads past EOF, so the tail is already silent.
                const CHUNK: usize = 1 << 16;
                let channels = (0..nch)
                    .map(|ch| {
                        let mut c = vec![0.0f32; frames];
                        let mut done = 0usize;
                        while done < frames {
                            let n = (frames - done).min(CHUNK);
                            if let Some(dst) = c.get_mut(done..done + n) {
                                self.read(ch, done as u64, dst);
                            }
                            done += n;
                        }
                        c
                    })
                    .collect();
                AudioBuffer { sample_rate: sr, channels }
            }
        }
    }

    /// The streamed source's file path, or `None` for a resident source.
    pub fn stream_path(&self) -> Option<&str> {
        match &self.backing {
            Backing::Resident => None,
            Backing::Streamed(s) => Some(&s.path),
        }
    }

    /// The streamed source's file format, or `None` for a resident source.
    pub fn stream_format(&self) -> Option<FileFormat> {
        match &self.backing {
            Backing::Resident => None,
            Backing::Streamed(s) => Some(s.format),
        }
    }

    /// Copy up to `out.len()` frames of channel `ch`, starting at frame `start`, into `out`,
    /// converting to `f32`. Works for both resident sources (a copy from the in-memory buffer) and
    /// streamed ones (an on-demand read from the file). Out-of-range channels, reads past the end
    /// and truncated files leave the untouched tail of `out` zeroed. Returns the number of real
    /// frames written (0 ..= `out.len()`); the rest of `out` is zero-padded. Never panics.
    pub fn read(&self, ch: usize, start: u64, out: &mut [f32]) -> usize {
        match &self.backing {
            Backing::Streamed(s) => s.reader.read_frames(ch, start, out),
            Backing::Resident => {
                for v in out.iter_mut() {
                    *v = 0.0;
                }
                let Some(chan) = self.buffer.channels.get(ch) else {
                    return 0;
                };
                let start = usize::try_from(start).unwrap_or(usize::MAX);
                let Some(src) = chan.get(start..) else {
                    return 0;
                };
                let n = out.len().min(src.len());
                if let (Some(dst), Some(src)) = (out.get_mut(..n), src.get(..n)) {
                    dst.copy_from_slice(src);
                }
                n
            }
        }
    }
}

/// Shared decoded audio, keyed by source id.
#[derive(Debug, Clone, Default)]
pub struct SourcePool {
    map: BTreeMap<SourceId, Arc<SourceAudio>>,
}

impl SourcePool {
    pub fn insert(&mut self, id: SourceId, audio: Arc<SourceAudio>) {
        self.map.insert(id, audio);
    }
    pub fn get(&self, id: SourceId) -> Option<&Arc<SourceAudio>> {
        self.map.get(&id)
    }
    pub fn remove(&mut self, id: SourceId) -> Option<Arc<SourceAudio>> {
        self.map.remove(&id)
    }
    pub fn contains(&self, id: SourceId) -> bool {
        self.map.contains_key(&id)
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn ids(&self) -> impl Iterator<Item = SourceId> + '_ {
        self.map.keys().copied()
    }
}

impl PartialEq for SourcePool {
    fn eq(&self, other: &Self) -> bool {
        self.map.len() == other.map.len() && self.map.iter().zip(other.map.iter()).all(|((a, x), (b, y))| a == b && Arc::ptr_eq(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundcraft_audio_io::{BitDepth, EncodeOptions, StreamReader};
    use std::io::Cursor;

    fn tone(frames: usize, k: f32) -> Vec<f32> {
        (0..frames).map(|i| (i as f32 * k).sin() * 0.8).collect()
    }

    fn streamed_source(left: &[f32], right: &[f32]) -> SourceAudio {
        let buf = AudioBuffer { sample_rate: 48_000, channels: vec![left.to_vec(), right.to_vec()] };
        let opts = EncodeOptions { format: FileFormat::Wav, bit_depth: BitDepth::Float32, dither: false, bwf: None };
        let bytes = soundcraft_audio_io::encode(&buf, &opts).expect("encode");
        let len = bytes.len() as u64;
        let reader = StreamReader::from_source(Cursor::new(bytes), len, Some("wav")).expect("open");
        let peaks = reader.build_peaks().expect("peaks");
        SourceAudio::streamed(reader, "audio/tone.wav", FileFormat::Wav, peaks)
    }

    #[test]
    fn resident_keeps_the_old_api() {
        let left = tone(500, 0.03);
        let right = tone(500, 0.05);
        let audio = SourceAudio::new(AudioBuffer { sample_rate: 44_100, channels: vec![left.clone(), right.clone()] });
        assert!(audio.is_resident());
        assert_eq!(audio.frames(), 500);
        assert_eq!(audio.num_channels(), 2);
        assert_eq!(audio.sample_rate(), 44_100);
        assert_eq!(audio.channel(0), Some(left.as_slice()));
        assert_eq!(audio.channel(1), Some(right.as_slice()));
        assert_eq!(audio.channel(2), None);
        // The public fields still exist and hold the resident data.
        assert_eq!(audio.buffer.channels[0], left);
        assert_eq!(audio.peaks.len(), 2);
        assert_eq!(audio.stream_path(), None);
        assert_eq!(audio.stream_format(), None);
    }

    #[test]
    fn resident_read_copies_and_zero_pads() {
        let mono = tone(100, 0.1);
        let audio = SourceAudio::new(AudioBuffer { sample_rate: 48_000, channels: vec![mono.clone()] });
        let mut out = vec![9.0f32; 30];
        assert_eq!(audio.read(0, 10, &mut out), 30);
        assert_eq!(out, mono[10..40]);
        // Past the end: partial fill then zero pad.
        let mut out = vec![9.0f32; 20];
        assert_eq!(audio.read(0, 90, &mut out), 10);
        assert_eq!(&out[..10], &mono[90..100]);
        assert!(out[10..].iter().all(|&v| v == 0.0));
        // Bad channel.
        let mut out = vec![9.0f32; 4];
        assert_eq!(audio.read(3, 0, &mut out), 0);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn streamed_has_no_slices_but_reads_and_matches_resident() {
        let left = tone(1200, 0.013);
        let right = tone(1200, 0.017);
        let streamed = streamed_source(&left, &right);
        assert!(!streamed.is_resident());
        assert_eq!(streamed.frames(), 1200);
        assert_eq!(streamed.num_channels(), 2);
        assert_eq!(streamed.sample_rate(), 48_000);
        // No slice API for streamed sources.
        assert_eq!(streamed.channel(0), None);
        // But read() yields the same samples a resident source would.
        let resident = SourceAudio::new(AudioBuffer { sample_rate: 48_000, channels: vec![left.clone(), right.clone()] });
        let mut a = vec![0.0f32; 400];
        let mut b = vec![0.0f32; 400];
        assert_eq!(streamed.read(0, 300, &mut a), 400);
        assert_eq!(resident.read(0, 300, &mut b), 400);
        assert_eq!(a, b);
        assert_eq!(a, left[300..700]);
        // Streamed carries its path and format; its buffer holds no samples but the right rate.
        assert_eq!(streamed.stream_path(), Some("audio/tone.wav"));
        assert_eq!(streamed.stream_format(), Some(FileFormat::Wav));
        assert!(streamed.buffer.channels.is_empty());
        assert_eq!(streamed.buffer.sample_rate, 48_000);
        assert_eq!(streamed.peaks.len(), 2);
    }

    #[test]
    fn load_resident_matches_the_samples() {
        let left = tone(900, 0.013);
        let right = tone(900, 0.021);
        let streamed = streamed_source(&left, &right);
        assert!(!streamed.is_resident());
        let buf = streamed.load_resident();
        assert_eq!(buf.sample_rate, 48_000);
        assert_eq!(buf.channels.len(), 2);
        assert_eq!(buf.channels[0], left);
        assert_eq!(buf.channels[1], right);
        // A resident source just clones.
        let resident = SourceAudio::new(AudioBuffer { sample_rate: 44_100, channels: vec![left.clone()] });
        assert_eq!(resident.load_resident().channels[0], left);
    }

    #[test]
    fn pool_still_compares_by_identity() {
        let a = Arc::new(SourceAudio::new(AudioBuffer::new(48_000, 1, 10)));
        let mut p1 = SourcePool::default();
        let mut p2 = SourcePool::default();
        p1.insert(SourceId(1), a.clone());
        p2.insert(SourceId(1), a.clone());
        assert_eq!(p1, p2);
        p2.insert(SourceId(2), Arc::new(SourceAudio::new(AudioBuffer::new(48_000, 1, 10))));
        assert_ne!(p1, p2);
    }
}
