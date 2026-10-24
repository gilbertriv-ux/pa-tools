//! Bounded native WAV/RF64/AIFF readers and a safe atomic read-ahead cache.
//!
//! File handles are capabilities opened by the engine after its path checks. No path is opened
//! here. The realtime `sample` path only accesses atomics; disk reads run on one shared worker.
//! All original files remain untouched. Unsupported compressed formats never trigger whole-file
//! decoding. Audio payload cache allocations share a strict 64 MiB global reservation.

use crate::pcm::{PcmKind, be_u16, be_u32, deinterleave, le_u16, le_u32, le_u64};
use crate::{AudioBuffer, AudioError, AudioInfo, BwfInfo, FileFormat, Result, SampleFormat};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, SystemTime};

pub const BLOCK_FRAMES: usize = 4096;
pub const CACHE_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const SLOTS: usize = 4;
const MAX_CHANNELS: usize = 64;
const EMPTY: u64 = u64::MAX;
static CACHE_BYTES: AtomicUsize = AtomicUsize::new(0);
static SCRATCH_BYTES: AtomicUsize = AtomicUsize::new(0);
pub const SCRATCH_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const READER_HEADROOM_BYTES: usize = 4 * 1024 * 1024;

/// Shared worker-memory permit. Hold it for the lifetime of the accounted buffers.
/// Large codec/SRC jobs leave 4 MiB available for the native source reads they require,
/// preventing converters from reserving the entire budget and waiting on each other forever.
/// This reserves memory only; it neither creates nor deletes any file.
pub struct ScratchReservation(usize);
impl ScratchReservation {
    pub fn new(bytes: usize) -> Result<Self> {
        reserve_scratch(&SCRATCH_BYTES, bytes)?;
        Ok(Self(bytes))
    }
}
fn reserve_scratch(counter: &AtomicUsize, bytes: usize) -> Result<()> {
    let limit = if bytes >= READER_HEADROOM_BYTES { SCRATCH_LIMIT_BYTES - READER_HEADROOM_BYTES } else { SCRATCH_LIMIT_BYTES };
    if bytes > limit {
        return Err(AudioError::TooLarge("scratch request exceeds the shared worker budget".into()));
    }
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(bytes).filter(|total| *total <= limit))
        .map_err(|_| AudioError::ScratchBusy)?;
    Ok(())
}
impl Drop for ScratchReservation {
    fn drop(&mut self) {
        SCRATCH_BYTES.fetch_sub(self.0, Ordering::AcqRel);
    }
}

pub fn global_cache_bytes() -> usize {
    CACHE_BYTES.load(Ordering::Relaxed)
}

struct Reservation(usize);
impl Reservation {
    fn new(bytes: usize) -> Result<Self> {
        CACHE_BYTES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(bytes).filter(|total| *total <= CACHE_LIMIT_BYTES))
            .map_err(|_| AudioError::TooLarge("audio read-ahead cache budget exhausted".into()))?;
        Ok(Self(bytes))
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        CACHE_BYTES.fetch_sub(self.0, Ordering::AcqRel);
    }
}

struct Slot {
    sequence: AtomicU64,
    block: AtomicU64,
    request: AtomicU64,
    samples: Box<[AtomicU32]>,
}

struct Layout {
    info: AudioInfo,
    kind: PcmKind,
    data_start: u64,
    block_align: usize,
    file_len: u64,
    modified: Option<SystemTime>,
}

/// A bounded file source. Debug deliberately excludes the open file capability and cache data.
pub struct FileAudio {
    file: Mutex<File>,
    layout: Layout,
    slots: [Slot; SLOTS],
    reservation: Reservation,
    failure: Mutex<Option<String>>,
    missed: AtomicU64,
    failed: AtomicBool,
}
impl std::fmt::Debug for FileAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileAudio").field("info", &self.layout.info).field("cache_bytes", &self.reservation.0).finish()
    }
}

struct Registry {
    sources: Mutex<Vec<Weak<FileAudio>>>,
}
static REGISTRY: OnceLock<std::result::Result<Arc<Registry>, String>> = OnceLock::new();
fn registry() -> Result<&'static Arc<Registry>> {
    REGISTRY
        .get_or_init(|| {
            let registry = Arc::new(Registry { sources: Mutex::new(Vec::new()) });
            let worker = Arc::clone(&registry);
            std::thread::Builder::new()
                .name("audio-prefetch".into())
                .spawn(move || {
                    loop {
                        let sources: Vec<_> = {
                            let mut entries = worker.sources.lock().unwrap_or_else(PoisonError::into_inner);
                            entries.retain(|source| source.strong_count() != 0);
                            entries.iter().filter_map(Weak::upgrade).collect()
                        };
                        for source in sources {
                            for slot in &source.slots {
                                let request = slot.request.swap(EMPTY, Ordering::AcqRel);
                                if request != EMPTY && source.cached(0, request.saturating_mul(BLOCK_FRAMES as u64)).is_none() {
                                    source.load_block(request);
                                }
                            }
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(registry)
        })
        .as_ref()
        .map_err(|e| AudioError::Unsupported(format!("audio prefetch worker: {e}")))
}

impl FileAudio {
    pub fn open(mut file: File) -> Result<Arc<Self>> {
        if !file.metadata()?.is_file() {
            return Err(AudioError::Unsupported("audio source must be a regular file".into()));
        }
        let layout = parse_layout(&mut file)?;
        let channels = usize::from(layout.info.channels);
        let samples = BLOCK_FRAMES.checked_mul(channels).ok_or_else(|| AudioError::TooLarge("cache sample count overflow".into()))?;
        let bytes =
            samples.checked_mul(SLOTS * std::mem::size_of::<AtomicU32>()).ok_or_else(|| AudioError::TooLarge("cache size overflow".into()))?;
        let reservation = Reservation::new(bytes)?;
        let registry = registry()?;
        let source = Arc::new(Self {
            file: Mutex::new(file),
            layout,
            slots: std::array::from_fn(|_| Slot {
                sequence: AtomicU64::new(0),
                block: AtomicU64::new(EMPTY),
                request: AtomicU64::new(EMPTY),
                samples: (0..samples).map(|_| AtomicU32::new(0)).collect(),
            }),
            reservation,
            failure: Mutex::new(None),
            missed: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        });
        registry.sources.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::downgrade(&source));
        source.prefetch(0);
        Ok(source)
    }

    pub fn info(&self) -> &AudioInfo {
        &self.layout.info
    }
    pub fn cache_bytes(&self) -> usize {
        self.reservation.0
    }
    pub fn underruns(&self) -> u64 {
        self.missed.load(Ordering::Relaxed)
    }
    pub fn has_error(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
    pub fn error(&self) -> Option<String> {
        self.failure.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Bounded synchronous block read. Never call from the audio callback.
    pub fn read_frames(&self, start: u64, frames: usize) -> Result<AudioBuffer> {
        let result = self.read_frames_inner(start, frames);
        if let Err(error) = &result
            && !matches!(error, AudioError::ScratchBusy)
        {
            self.failed.store(true, Ordering::Relaxed);
            *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error.to_string());
        }
        result
    }

    fn read_frames_inner(&self, start: u64, frames: usize) -> Result<AudioBuffer> {
        if frames > BLOCK_FRAMES {
            return Err(AudioError::TooLarge("a file read exceeds 4096 frames".into()));
        }
        let frames = usize::try_from(self.layout.info.frames.saturating_sub(start).min(frames as u64))
            .map_err(|_| AudioError::TooLarge("frame count overflow".into()))?;
        if frames == 0 {
            return Ok(AudioBuffer::new(self.layout.info.sample_rate, usize::from(self.layout.info.channels), 0));
        }
        let bytes = frames.checked_mul(self.layout.block_align).ok_or_else(|| AudioError::TooLarge("read size overflow".into()))?;
        let output_bytes = frames
            .checked_mul(usize::from(self.layout.info.channels))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| AudioError::TooLarge("decoded block size overflow".into()))?;
        let _scratch = ScratchReservation::new(bytes.saturating_add(output_bytes))?;
        let offset = start
            .checked_mul(self.layout.block_align as u64)
            .and_then(|n| self.layout.data_start.checked_add(n))
            .ok_or_else(|| AudioError::TooLarge("audio seek overflow".into()))?;
        let file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        let meta = file.metadata()?;
        if meta.len() != self.layout.file_len || (self.layout.modified.is_some() && meta.modified().ok() != self.layout.modified) {
            return Err(AudioError::Malformed("audio source changed after opening".into()));
        }
        let mut raw = vec![0u8; bytes];
        read_positioned(&file, &mut raw, offset)?;
        let channels = deinterleave(&raw, self.layout.kind, usize::from(self.layout.info.channels), frames as u64)?;
        Ok(AudioBuffer { sample_rate: self.layout.info.sample_rate, channels })
    }

    /// Schedule the current and two following blocks; no allocation, locking, wake or I/O.
    pub fn prefetch(&self, frame: u64) {
        let block = frame / BLOCK_FRAMES as u64;
        for n in 0..3u64 {
            let target = block.saturating_add(n);
            if target.saturating_mul(BLOCK_FRAMES as u64) < self.layout.info.frames {
                if let Some(slot) = self.slots.get((target % SLOTS as u64) as usize) {
                    slot.request.store(target, Ordering::Release);
                }
            }
        }
    }

    fn cached(&self, channel: usize, frame: u64) -> Option<f32> {
        let block = frame / BLOCK_FRAMES as u64;
        let slot = self.slots.get((block % SLOTS as u64) as usize)?;
        let sequence = slot.sequence.load(Ordering::Acquire);
        if sequence & 1 != 0 || slot.block.load(Ordering::Relaxed) != block {
            return None;
        }
        let offset = (frame % BLOCK_FRAMES as u64) as usize;
        let index = channel.checked_mul(BLOCK_FRAMES)?.checked_add(offset)?;
        let bits = slot.samples.get(index)?.load(Ordering::Relaxed);
        // Keep payload loads before the final version check, including on weakly ordered CPUs.
        fence(Ordering::SeqCst);
        (slot.sequence.load(Ordering::Acquire) == sequence).then(|| f32::from_bits(bits))
    }

    /// Realtime misses return None and queue prefetch; offline misses read synchronously.
    /// Outside the source/channel extent is None rather than an I/O error.
    pub fn sample(&self, channel: usize, frame: u64, realtime: bool) -> Option<f32> {
        if channel >= usize::from(self.layout.info.channels) || frame >= self.layout.info.frames {
            return None;
        }
        if frame.is_multiple_of(BLOCK_FRAMES as u64) {
            self.prefetch(frame);
        }
        if let Some(sample) = self.cached(channel, frame) {
            return Some(sample);
        }
        if realtime {
            self.prefetch(frame);
            self.missed.fetch_add(1, Ordering::Relaxed);
            None
        } else {
            // Only an offline caller may wait for a simultaneous cache publisher.
            // Never turn writer contention into silence in an offline render.
            loop {
                self.load_block(frame / BLOCK_FRAMES as u64);
                if let Some(sample) = self.cached(channel, frame) {
                    break Some(sample);
                }
                if self.has_error() {
                    break None;
                }
                std::thread::yield_now();
            }
        }
    }

    fn load_block(&self, block: u64) {
        let Some(slot) = self.slots.get((block % SLOTS as u64) as usize) else { return };
        // One writer owns a slot until all samples and its block identity have been published.
        let sequence = slot.sequence.load(Ordering::Acquire);
        if sequence & 1 != 0 || slot.sequence.compare_exchange(sequence, sequence.wrapping_add(1), Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        let start = block.saturating_mul(BLOCK_FRAMES as u64);
        match self.read_frames(start, BLOCK_FRAMES) {
            Ok(audio) => {
                for (channel, samples) in audio.channels.iter().enumerate() {
                    let begin = channel * BLOCK_FRAMES;
                    for offset in 0..BLOCK_FRAMES {
                        if let Some(target) = slot.samples.get(begin + offset) {
                            target.store(samples.get(offset).copied().unwrap_or(0.0).to_bits(), Ordering::Relaxed);
                        }
                    }
                }
                slot.block.store(block, Ordering::Relaxed);
            }
            Err(AudioError::ScratchBusy) => {
                // Do not discard a still-valid cached page or classify temporary admission
                // pressure as corrupted media. Preserve any newer request already queued.
                let _ = slot.request.compare_exchange(EMPTY, block, Ordering::Release, Ordering::Relaxed);
            }
            Err(error) => {
                *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error.to_string());
                slot.block.store(EMPTY, Ordering::Relaxed);
            }
        }
        slot.sequence.store(sequence.wrapping_add(2), Ordering::Release);
    }
}

// Positioned reads do not race file cursors belonging to File::try_clone capabilities.
fn read_positioned(file: &File, mut bytes: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !bytes.is_empty() {
        #[cfg(unix)]
        let n = {
            use std::os::unix::fs::FileExt;
            file.read_at(bytes, offset)?
        };
        #[cfg(windows)]
        let n = {
            use std::os::windows::fs::FileExt;
            file.seek_read(bytes, offset)?
        };
        #[cfg(not(any(unix, windows)))]
        let n = {
            let mut clone = file.try_clone()?;
            clone.seek(SeekFrom::Start(offset))?;
            clone.read(bytes)?
        };
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short audio read"));
        }
        offset = offset.checked_add(n as u64).ok_or_else(|| std::io::Error::other("audio offset overflow"))?;
        bytes = bytes.get_mut(n..).ok_or_else(|| std::io::Error::other("audio read length overflow"))?;
    }
    Ok(())
}

fn malformed(message: &str) -> AudioError {
    AudioError::Malformed(message.into())
}
fn parse_layout(file: &mut File) -> Result<Layout> {
    let metadata = file.metadata()?;
    let file_len = metadata.len();
    let mut header = [0u8; 12];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut header)?;
    if header.get(..4) == Some(&b"FORM"[..]) && matches!(header.get(8..12), Some(b"AIFF") | Some(b"AIFC")) {
        return parse_aiff_layout(file, &header, metadata);
    }
    let rf64 = header.get(..4) == Some(&b"RF64"[..]) || header.get(..4) == Some(&b"BW64"[..]);
    if (!rf64 && header.get(..4) != Some(&b"RIFF"[..])) || header.get(8..12) != Some(&b"WAVE"[..]) {
        return Err(AudioError::Unsupported("disk reader supports native PCM/float WAV, RF64, AIFF and AIFF-C".into()));
    }
    let mut at = 12u64;
    let mut fmt = None;
    let mut data = None;
    let mut data64 = None;
    let mut bwf = None;
    let mut chunks = 0usize;
    while at.checked_add(8).is_some_and(|n| n <= file_len) {
        chunks += 1;
        if chunks > 100_000 {
            return Err(malformed("too many WAV chunks"));
        }
        file.seek(SeekFrom::Start(at))?;
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk)?;
        let declared = u64::from(le_u32(&chunk, 4).ok_or_else(|| malformed("short chunk size"))?);
        let body = at.checked_add(8).ok_or_else(|| malformed("chunk offset overflow"))?;
        let is_data = chunk.get(..4) == Some(&b"data"[..]);
        let size = if is_data && rf64 && declared == u64::from(u32::MAX) {
            data64.ok_or_else(|| malformed("RF64 data requires preceding ds64"))?
        } else {
            declared
        };
        let end = body.checked_add(size).filter(|end| *end <= file_len).ok_or_else(|| malformed("WAV chunk exceeds file length"))?;
        match chunk.get(..4) {
            Some(b"fmt ") => {
                if size < 16 {
                    return Err(malformed("short WAV fmt"));
                }
                let mut bytes = [0u8; 64];
                let count = size.min(bytes.len() as u64) as usize;
                file.read_exact(bytes.get_mut(..count).ok_or_else(|| malformed("fmt length"))?)?;
                let mut tag = le_u16(&bytes, 0).ok_or_else(|| malformed("format tag"))?;
                let channels = le_u16(&bytes, 2).ok_or_else(|| malformed("channel count"))?;
                let rate = le_u32(&bytes, 4).ok_or_else(|| malformed("sample rate"))?;
                let align = usize::from(le_u16(&bytes, 12).ok_or_else(|| malformed("frame alignment"))?);
                let bits = le_u16(&bytes, 14).ok_or_else(|| malformed("sample width"))?;
                if tag == 0xfffe {
                    let tail = [0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71];
                    if size < 40 || bytes.get(26..40) != Some(&tail[..]) {
                        return Err(malformed("unsupported WAV subtype GUID"));
                    }
                    tag = le_u16(&bytes, 24).ok_or_else(|| malformed("WAV subtype"))?;
                    let valid = le_u16(&bytes, 18).unwrap_or(bits);
                    if valid > bits {
                        return Err(malformed("invalid WAV valid-bit count"));
                    }
                }
                if channels == 0 || usize::from(channels) > MAX_CHANNELS || !(8_000..=768_000).contains(&rate) {
                    return Err(AudioError::Unsupported("disk source channels/rate outside supported bounds".into()));
                }
                let width = usize::from(bits / 8);
                let (kind, sample_format) = match (tag, bits) {
                    (1, 8) => (PcmKind::Int { bytes: 1, big_endian: false, unsigned: true }, SampleFormat::Int8),
                    (1, 16) => (PcmKind::Int { bytes: 2, big_endian: false, unsigned: false }, SampleFormat::Int16),
                    (1, 24) => (PcmKind::Int { bytes: 3, big_endian: false, unsigned: false }, SampleFormat::Int24),
                    (1, 32) => (PcmKind::Int { bytes: 4, big_endian: false, unsigned: false }, SampleFormat::Int32),
                    (3, 32) => (PcmKind::Float { bytes: 4, big_endian: false }, SampleFormat::Float32),
                    (3, 64) => (PcmKind::Float { bytes: 8, big_endian: false }, SampleFormat::Float64),
                    _ => return Err(AudioError::Unsupported("WAV codec is not native PCM/float".into())),
                };
                if align != usize::from(channels).saturating_mul(width) {
                    return Err(malformed("WAV block alignment disagrees with channels/sample width"));
                }
                fmt = Some((channels, rate, align, kind, sample_format));
            }
            Some(b"ds64") if rf64 => {
                if size < 28 {
                    return Err(malformed("short RF64 ds64"));
                }
                let mut bytes = [0u8; 28];
                file.read_exact(&mut bytes)?;
                data64 = le_u64(&bytes, 8);
            }
            Some(b"data") => {
                if data.is_none() {
                    data = Some((body, size));
                }
            }
            Some(b"bext") if size >= 346 => {
                let mut bytes = [0u8; 346];
                file.read_exact(&mut bytes)?;
                let text = |start, end| {
                    let field = bytes.get(start..end).unwrap_or_default();
                    let n = field.iter().position(|b| *b == 0).unwrap_or(field.len());
                    String::from_utf8_lossy(field.get(..n).unwrap_or_default()).trim_end().to_string()
                };
                bwf = Some(BwfInfo {
                    description: text(0, 256),
                    originator: text(256, 288),
                    originator_reference: text(288, 320),
                    origination_date: text(320, 330),
                    origination_time: text(330, 338),
                    time_reference: le_u64(&bytes, 338).unwrap_or(0),
                });
            }
            _ => {}
        }
        at = end.checked_add(size & 1).ok_or_else(|| malformed("chunk padding overflow"))?;
    }
    let (channels, sample_rate, block_align, kind, sample_format) = fmt.ok_or_else(|| malformed("missing WAV fmt"))?;
    let (data_start, data_len) = data.ok_or_else(|| malformed("missing WAV data"))?;
    if data_len % block_align as u64 != 0 {
        return Err(malformed("WAV data ends in a partial frame"));
    }
    Ok(Layout {
        info: AudioInfo { format: FileFormat::Wav, sample_format, sample_rate, channels, frames: data_len / block_align as u64, bwf },
        kind,
        data_start,
        block_align,
        file_len,
        modified: metadata.modified().ok(),
    })
}

fn parse_aiff_layout(file: &mut File, header: &[u8; 12], metadata: std::fs::Metadata) -> Result<Layout> {
    let file_len = metadata.len();
    let form_len = u64::from(be_u32(header, 4).ok_or_else(|| malformed("missing AIFF FORM size"))?);
    let end = form_len.checked_add(8).filter(|end| *end >= 12 && *end <= file_len).ok_or_else(|| malformed("AIFF FORM exceeds file length"))?;
    let aifc = header.get(8..12) == Some(&b"AIFC"[..]);
    let mut at = 12u64;
    let mut comm = None;
    let mut sound = None;
    let mut chunks = 0usize;
    while at.checked_add(8).is_some_and(|n| n <= end) {
        chunks += 1;
        if chunks > 100_000 {
            return Err(malformed("too many AIFF chunks"));
        }
        file.seek(SeekFrom::Start(at))?;
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk)?;
        let size = u64::from(be_u32(&chunk, 4).ok_or_else(|| malformed("short AIFF chunk size"))?);
        let body = at.checked_add(8).ok_or_else(|| malformed("AIFF chunk position overflow"))?;
        let chunk_end = body.checked_add(size).filter(|last| *last <= end).ok_or_else(|| malformed("AIFF chunk exceeds FORM length"))?;
        match chunk.get(..4) {
            Some(b"COMM") => {
                let required = if aifc { 22 } else { 18 };
                if size < required {
                    return Err(malformed("AIFF COMM is too short"));
                }
                let mut bytes = [0u8; 22];
                file.read_exact(bytes.get_mut(..required as usize).ok_or_else(|| malformed("AIFF COMM size"))?)?;
                let channels = be_u16(&bytes, 0).ok_or_else(|| malformed("AIFF channel count"))?;
                let frames = u64::from(be_u32(&bytes, 2).ok_or_else(|| malformed("AIFF frame count"))?);
                let bits = be_u16(&bytes, 6).ok_or_else(|| malformed("AIFF sample size"))?;
                let rate = crate::aiff::extended_to_f64(bytes.get(8..18).ok_or_else(|| malformed("AIFF sample rate"))?)
                    .filter(|rate| rate.is_finite() && (8_000.0..=768_000.0).contains(rate))
                    .ok_or_else(|| malformed("AIFF sample rate outside supported bounds"))?;
                if channels == 0 || usize::from(channels) > MAX_CHANNELS {
                    return Err(AudioError::Unsupported("AIFF channel count outside supported bounds".into()));
                }
                let compression: [u8; 4] = if aifc {
                    bytes.get(18..22).and_then(|bytes| bytes.try_into().ok()).ok_or_else(|| malformed("AIFF compression type"))?
                } else {
                    *b"NONE"
                };
                let integer = |big_endian| -> Result<(PcmKind, SampleFormat)> {
                    if !(1..=32).contains(&bits) {
                        return Err(AudioError::Unsupported("unsupported AIFF integer sample width".into()));
                    }
                    let width = usize::from(bits).div_ceil(8);
                    let format = match bits {
                        1..=8 => SampleFormat::Int8,
                        9..=16 => SampleFormat::Int16,
                        17..=24 => SampleFormat::Int24,
                        _ => SampleFormat::Int32,
                    };
                    Ok((PcmKind::Int { bytes: width, big_endian, unsigned: false }, format))
                };
                let (kind, sample_format) = match &compression {
                    b"NONE" | b"twos" => integer(true)?,
                    b"sowt" => integer(false)?,
                    b"fl32" | b"FL32" if bits == 32 => (PcmKind::Float { bytes: 4, big_endian: true }, SampleFormat::Float32),
                    b"fl64" | b"FL64" if bits == 64 => (PcmKind::Float { bytes: 8, big_endian: true }, SampleFormat::Float64),
                    _ => return Err(AudioError::Unsupported("AIFF-C codec is not native PCM/float".into())),
                };
                comm = Some((channels, frames, rate.round() as u32, kind, sample_format));
            }
            Some(b"SSND") if sound.is_none() => {
                if size < 8 {
                    return Err(malformed("AIFF SSND is too short"));
                }
                let mut bytes = [0u8; 8];
                file.read_exact(&mut bytes)?;
                let offset = u64::from(be_u32(&bytes, 0).ok_or_else(|| malformed("AIFF sound offset"))?);
                let prefix = offset.checked_add(8).filter(|prefix| *prefix <= size).ok_or_else(|| malformed("AIFF sound offset exceeds chunk"))?;
                let start = body.checked_add(prefix).ok_or_else(|| malformed("AIFF data offset overflow"))?;
                sound = Some((start, size - prefix));
            }
            _ => {}
        }
        at = chunk_end.checked_add(size & 1).ok_or_else(|| malformed("AIFF padding overflow"))?;
    }
    let (channels, frames, sample_rate, kind, sample_format) = comm.ok_or_else(|| malformed("AIFF has no COMM chunk"))?;
    let (data_start, data_len) = sound.ok_or_else(|| malformed("AIFF has no SSND chunk"))?;
    let block_align = usize::from(channels).checked_mul(kind.bytes()).ok_or_else(|| malformed("AIFF frame width overflow"))?;
    let required = frames.checked_mul(block_align as u64).ok_or_else(|| malformed("AIFF sample extent overflow"))?;
    if required > data_len {
        return Err(malformed("AIFF frame count exceeds sound data"));
    }
    Ok(Layout {
        info: AudioInfo { format: FileFormat::Aiff, sample_format, sample_rate, channels, frames, bwf: None },
        kind,
        data_start,
        block_align,
        file_len,
        modified: metadata.modified().ok(),
    })
}

#[cfg(test)]
mod aiff_file_tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture(bytes: &[u8]) -> File {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..1000 {
            let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("pa-disk-aiff-{}-{stamp}-{n}.aif", std::process::id()));
            match std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    eprintln!("CREATED {} (retained)", path.display());
                    file.write_all(bytes).unwrap();
                    return file;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create retained AIFF fixture: {error}"),
            }
        }
        panic!("cannot reserve retained AIFF fixture")
    }

    #[test]
    fn file_aiff_matches_existing_native_decode() {
        for depth in [crate::BitDepth::Int16, crate::BitDepth::Int24, crate::BitDepth::Int32, crate::BitDepth::Float32] {
            let samples = (0..5000).map(|n| ((n as f32) * 0.02).sin() * 0.75).collect::<Vec<_>>();
            let buffer = AudioBuffer { sample_rate: 44_100, channels: vec![samples.clone(), samples.iter().map(|sample| -*sample).collect()] };
            let bytes =
                crate::encode(&buffer, &crate::EncodeOptions { format: FileFormat::Aiff, bit_depth: depth, dither: false, bwf: None }).unwrap();
            let expected = crate::decode(&bytes, Some("aif")).unwrap().1;
            let source = FileAudio::open(fixture(&bytes)).unwrap();
            assert_eq!(source.info().format, FileFormat::Aiff);
            assert_eq!(source.info().frames, 5000);
            let block = source.read_frames(4000, 1000).unwrap();
            for channel in 0..2 {
                assert_eq!(block.channels[channel], expected.channels[channel][4000..]);
                assert_eq!(source.sample(channel, 4096, false), Some(expected.channels[channel][4096]));
            }
        }
    }

    #[test]
    fn aiff_sound_offset_is_honored_and_invalid_extent_rejected() {
        let input = AudioBuffer { sample_rate: 48_000, channels: vec![vec![0.25, -0.5]] };
        let options = crate::EncodeOptions { format: FileFormat::Aiff, bit_depth: crate::BitDepth::Int16, dither: false, bwf: None };
        let mut bytes = crate::encode(&input, &options).unwrap();
        let at = bytes.windows(4).position(|window| window == b"SSND").unwrap();
        let original_size = be_u32(&bytes, at + 4).unwrap();
        bytes.splice(at + 16..at + 16, [0xaa, 0xbb, 0xcc, 0xdd]);
        bytes.get_mut(at + 4..at + 8).unwrap().copy_from_slice(&(original_size + 4).to_be_bytes());
        bytes.get_mut(at + 8..at + 12).unwrap().copy_from_slice(&4u32.to_be_bytes());
        let form_size = bytes.len() as u32 - 8;
        bytes.get_mut(4..8).unwrap().copy_from_slice(&form_size.to_be_bytes());
        let source = FileAudio::open(fixture(&bytes)).unwrap();
        assert_eq!(source.read_frames(0, 2).unwrap().channels[0], [0.25, -0.5]);
        bytes.get_mut(at + 8..at + 12).unwrap().copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(FileAudio::open(fixture(&bytes)).is_err());
    }
}

#[cfg(test)]
mod scratch_budget_tests {
    use super::*;
    #[test]
    fn converter_admission_preserves_native_reader_headroom() {
        let counter = AtomicUsize::new(0);
        reserve_scratch(&counter, 8 * 1024 * 1024).unwrap();
        assert!(matches!(reserve_scratch(&counter, 8 * 1024 * 1024), Err(AudioError::ScratchBusy)));
        reserve_scratch(&counter, 4 * 1024 * 1024).unwrap();
        reserve_scratch(&counter, 3 * 1024 * 1024).unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 15 * 1024 * 1024);
        assert!(matches!(reserve_scratch(&counter, 2 * 1024 * 1024), Err(AudioError::ScratchBusy)));
        // A released converter allows another large worker reservation without global state.
        counter.fetch_sub(8 * 1024 * 1024, Ordering::Relaxed);
        reserve_scratch(&counter, 4 * 1024 * 1024).unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 11 * 1024 * 1024);
    }
    #[test]
    fn impossible_scratch_requests_are_errors_without_counter_mutation() {
        let counter = AtomicUsize::new(0);
        assert!(matches!(reserve_scratch(&counter, usize::MAX), Err(AudioError::TooLarge(_))));
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        let _zero_permit = ScratchReservation::new(0).unwrap();
    }
}
