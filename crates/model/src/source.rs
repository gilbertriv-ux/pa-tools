//! Audio sources (files in the session's Audio Files folder) and their decoded data, and the
//! movies video clips reference.

use crate::SourceId;
use soundcraft_audio_io::{AudioBuffer, FileFormat, peaks::Peaks};
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

/// Audio metadata and shared storage. Native file sources never own a whole-file sample Vec.
/// Resident storage remains available for small synthesized/test buffers.
pub struct SourceAudio {
    storage: AudioStorage,
    backing_path: Option<std::path::PathBuf>,
    pub peaks: Vec<Peaks>,
    waveform: soundcraft_audio_io::peak_stream::WaveformCache,
}
enum AudioStorage {
    Resident(AudioBuffer),
    File(Arc<soundcraft_audio_io::reader::FileAudio>),
}
impl std::fmt::Debug for SourceAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceAudio")
            .field("frames", &self.frames())
            .field("channels", &self.num_channels())
            .field("file_backed", &self.is_file_backed())
            .finish()
    }
}
impl SourceAudio {
    pub fn new(buffer: AudioBuffer) -> Self {
        let peaks = buffer.channels.iter().map(|c| Peaks::build(c)).collect();
        Self { storage: AudioStorage::Resident(buffer), backing_path: None, peaks, waveform: Default::default() }
    }
    pub fn from_file(file: Arc<soundcraft_audio_io::reader::FileAudio>) -> Self {
        Self { storage: AudioStorage::File(file), backing_path: None, peaks: Vec::new(), waveform: Default::default() }
    }
    pub fn from_file_at(file: Arc<soundcraft_audio_io::reader::FileAudio>, path: std::path::PathBuf) -> Self {
        let mut audio = Self::from_file(file);
        audio.backing_path = Some(path);
        audio
    }
    pub fn backing_path(&self) -> Option<&std::path::Path> {
        self.backing_path.as_deref()
    }
    pub fn file_info(&self) -> Option<&soundcraft_audio_io::AudioInfo> {
        match &self.storage {
            AudioStorage::File(file) => Some(file.info()),
            _ => None,
        }
    }
    /// Attach an exclusively-created peak index capability. Index generation starts on demand.
    pub fn attach_peak_index(&self,index:std::fs::File)->Result<(),String> {
        match &self.storage {
            AudioStorage::File(file)=>self.waveform.attach_index(Arc::clone(file),index),
            AudioStorage::Resident(_)=>Err("resident audio does not need a disk waveform index".into()),
        }
    }
    pub fn is_file_backed(&self) -> bool {
        matches!(&self.storage, AudioStorage::File(_))
    }
    pub fn resident(&self) -> Option<&AudioBuffer> {
        match &self.storage {
            AudioStorage::Resident(buffer) => Some(buffer),
            _ => None,
        }
    }
    pub fn sample_rate(&self) -> u32 {
        match &self.storage {
            AudioStorage::Resident(b) => b.sample_rate,
            AudioStorage::File(f) => f.info().sample_rate,
        }
    }
    pub fn num_channels(&self) -> usize {
        match &self.storage {
            AudioStorage::Resident(b) => b.num_channels(),
            AudioStorage::File(f) => usize::from(f.info().channels),
        }
    }
    pub fn frames(&self) -> usize {
        match &self.storage {
            AudioStorage::Resident(b) => b.frames(),
            AudioStorage::File(f) => usize::try_from(f.info().frames).unwrap_or(usize::MAX),
        }
    }
    /// Whole-channel borrowing is available only for resident storage.
    pub fn channel(&self, ch: usize) -> Option<&[f32]> {
        self.resident()?.channels.get(ch).map(Vec::as_slice)
    }
    pub fn sample(&self, ch: usize, frame: usize, realtime: bool) -> Option<f32> {
        match &self.storage {
            AudioStorage::Resident(b) => b.channels.get(ch)?.get(frame).copied(),
            AudioStorage::File(f) => f.sample(ch, frame as u64, realtime),
        }
    }
    pub fn prefetch(&self, frame: usize) {
        if let AudioStorage::File(file) = &self.storage {
            file.prefetch(frame as u64);
        }
    }
    pub fn stream_error(&self) -> Option<String> {
        match &self.storage {
            AudioStorage::File(f) => f.error(),
            _ => None,
        }
    }
    pub fn underruns(&self) -> u64 {
        match &self.storage {
            AudioStorage::File(f) => f.underruns(),
            _ => 0,
        }
    }
    /// A bounded control/offline read, never called from the audio callback.
    pub fn read_range(&self, start: usize, frames: usize) -> Result<AudioBuffer, String> {
        if frames.saturating_mul(self.num_channels()) > 4 * 1024 * 1024 {
            return Err("audio read exceeds the 16 MiB block budget".into());
        }
        let n = frames.min(self.frames().saturating_sub(start));
        let mut out = AudioBuffer::new(self.sample_rate(), self.num_channels(), n);
        match &self.storage {
            AudioStorage::Resident(buffer) => {
                for (dst, src) in out.channels.iter_mut().zip(&buffer.channels) {
                    if let Some(part) = src.get(start..start.saturating_add(n)) {
                        dst.copy_from_slice(part);
                    }
                }
            }
            AudioStorage::File(file) => {
                for offset in (0..n).step_by(4096) {
                    let count = (n - offset).min(4096);
                    let block = file.read_frames((start + offset) as u64, count).map_err(|e| e.to_string())?;
                    for (dst, src) in out.channels.iter_mut().zip(&block.channels) {
                        if let Some(to) = dst.get_mut(offset..offset + count) {
                            if to.len() != src.len() {
                                return Err("short source read".into());
                            }
                            to.copy_from_slice(src);
                        }
                    }
                }
            }
        }
        Ok(out)
    }
    /// Explicit bounded materialization for small legacy/offline operations; never a fallback.
    pub fn materialize(&self, limit_samples: usize) -> Result<AudioBuffer, String> {
        if self.frames().saturating_mul(self.num_channels()) > limit_samples.min(4 * 1024 * 1024) {
            return Err("operation exceeds the bounded resident-audio budget".into());
        }
        self.read_range(0, self.frames())
    }
    /// Encode through bounded blocks on a control/offline worker. Never use from a callback.
    pub fn encode_to<W: std::io::Write + std::io::Seek>(&self, output: W, options: &soundcraft_audio_io::EncodeOptions) -> Result<W, String> {
        let mut encoder =
            soundcraft_audio_io::FileEncoder::new(output, options, self.num_channels(), self.sample_rate(), None).map_err(|e| e.to_string())?;
        for start in (0..self.frames()).step_by(4096) {
            let block = self.read_range(start, (self.frames() - start).min(4096))?;
            encoder.write(&block.channels, block.frames()).map_err(|e| e.to_string())?;
        }
        encoder.finish().map_err(|e| e.to_string())
    }

    /// Returns a ready disk waveform tile, or queues work and returns None without waiting.
    pub fn waveform_spans(&self, ch: usize, start: f64, step: f64, overlap: f64, columns: usize, power: bool) -> Option<Vec<Option<(f32, f32)>>> {
        match &self.storage {
            AudioStorage::Resident(buffer) => {
                let samples = buffer.channels.get(ch)?;
                let peaks = self.peaks.get(ch)?;
                Some(if power {
                    peaks.rms_spans(samples, start, step, overlap, columns).into_iter().map(|v| v.map(|x| (-x, x))).collect()
                } else {
                    peaks.spans(samples, start, step, overlap, columns)
                })
            }
            AudioStorage::File(file) => {
                self.waveform.query(Arc::clone(file), ch, start, step, overlap, columns, power).map(|tile| tile.spans.clone())
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
