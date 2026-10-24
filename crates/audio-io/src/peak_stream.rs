//! Bounded asynchronous waveform queries for file-backed audio.
//! One worker evaluates requests in bounded blocks. New viewport requests cancel obsolete work
//! per source/channel; a global LRU releases old in-memory tiles before reserving replacements.
use crate::reader::FileAudio;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

const LIMIT: usize = 8 * 1024 * 1024;
const MAX_COLUMNS: usize = 4096;
const MAX_CHANNELS: usize = 64;
const MAX_TILES: usize = 128;
static BYTES: AtomicUsize = AtomicUsize::new(0);
static NEXT_CACHE: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    channel: usize,
    start: u64,
    step: u64,
    overlap: u64,
    columns: usize,
    power: bool,
}
pub struct WaveformTile {
    pub spans: Vec<Option<(f32, f32)>>,
    reserved: usize,
}
impl Drop for WaveformTile {
    fn drop(&mut self) {
        BYTES.fetch_sub(self.reserved, Ordering::Relaxed);
    }
}
struct RequestState {
    latest: [Option<Key>; MAX_CHANNELS],
    pending: [Option<(Key, u64)>; MAX_CHANNELS],
}
impl Default for RequestState {
    fn default() -> Self {
        Self { latest: [None; MAX_CHANNELS], pending: [None; MAX_CHANNELS] }
    }
}
struct Shared {
    id: u64,
    generations: [AtomicU64; MAX_CHANNELS],
    state: Mutex<RequestState>,
    index: Mutex<Option<Arc<disk_index::Index>>>,
    cancelled: Arc<AtomicBool>,
}
impl Default for Shared {
    fn default() -> Self {
        Self {
            id: NEXT_CACHE.fetch_add(1, Ordering::Relaxed),
            generations: std::array::from_fn(|_| AtomicU64::new(0)),
            state: Mutex::new(RequestState::default()),
            index: Mutex::new(None),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
}
#[derive(Default)]
pub struct WaveformCache {
    shared: Arc<Shared>,
}
impl Drop for WaveformCache {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::Release);
        // A closed source must not keep either worker scanning an obsolete viewport.
        for generation in &self.shared.generations {
            generation.fetch_add(1, Ordering::Release);
        }
    }
}

struct Entry {
    cache: u64,
    key: Key,
    tile: Arc<WaveformTile>,
}
fn tiles() -> &'static Mutex<VecDeque<Entry>> {
    static TILES: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
    TILES.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_TILES)))
}
struct Job {
    file: Arc<FileAudio>,
    key: Key,
    generation: u64,
    shared: Arc<Shared>,
}
impl Job {
    fn current(&self) -> bool {
        self.shared.generations.get(self.key.channel).is_some_and(|generation| generation.load(Ordering::Acquire) == self.generation)
    }
}
fn worker() -> Option<&'static SyncSender<Job>> {
    static WORKER: OnceLock<Option<SyncSender<Job>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (tx, rx) = sync_channel::<Job>(32);
            std::thread::Builder::new()
                .name("audio-waveforms".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        let result = if job.current() { build(&job) } else { None };
                        if let Some(tile) = result.filter(|_| job.current()) {
                            let mut ready = tiles().lock().unwrap_or_else(PoisonError::into_inner);
                            if ready.len() >= MAX_TILES {
                                ready.pop_front();
                            }
                            ready.push_back(Entry { cache: job.shared.id, key: job.key, tile: Arc::new(tile) });
                        }
                        let mut state = job.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                        if let Some(pending) = state.pending.get_mut(job.key.channel)
                            && *pending == Some((job.key, job.generation))
                        {
                            *pending = None;
                        }
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}
impl WaveformCache {
    /// Attach an exclusively new, empty read/write sidecar capability. No source scanning or
    /// index writing occurs here; the first waveform query lazily schedules its background build.
    pub fn attach_index(&self, source: Arc<FileAudio>, output: std::fs::File) -> Result<(), String> {
        let index = disk_index::Index::new(source, output, Arc::clone(&self.shared.cancelled))?;
        let mut attached = self.shared.index.lock().unwrap_or_else(PoisonError::into_inner);
        if attached.is_some() {
            return Err("a waveform index is already attached".into());
        }
        *attached = Some(Arc::new(index));
        Ok(())
    }

    pub fn query(
        &self,
        file: Arc<FileAudio>,
        channel: usize,
        start: f64,
        step: f64,
        overlap: f64,
        columns: usize,
        power: bool,
    ) -> Option<Arc<WaveformTile>> {
        if !start.is_finite()
            || !step.is_finite()
            || step <= 0.0
            || !overlap.is_finite()
            || overlap < 0.0
            || channel >= usize::from(file.info().channels)
            || channel >= MAX_CHANNELS
        {
            return None;
        }
        if let Ok(attached) = self.shared.index.try_lock()
            && let Some(index) = attached.as_ref()
        {
            disk_index::schedule(index);
        }
        let key = Key { channel, start: start.to_bits(), step: step.to_bits(), overlap: overlap.to_bits(), columns: columns.min(MAX_COLUMNS), power };
        // Queries are UI/control operations. try_lock means neither disk nor another query can
        // stall a paint. Per-channel generations avoid different channel paints cancelling each other.
        let generation = {
            let Ok(mut state) = self.shared.state.try_lock() else {
                return None;
            };
            let latest = state.latest.get_mut(channel)?;
            if *latest != Some(key) {
                *latest = Some(key);
                self.shared.generations.get(channel)?.fetch_add(1, Ordering::AcqRel);
            }
            self.shared.generations.get(channel)?.load(Ordering::Acquire)
        };
        {
            let Ok(mut ready) = tiles().try_lock() else {
                return None;
            };
            if let Some(at) = ready.iter().position(|entry| entry.cache == self.shared.id && entry.key == key) {
                let entry = ready.remove(at)?;
                let tile = Arc::clone(&entry.tile);
                ready.push_back(entry);
                return Some(tile);
            }
        }
        let Ok(mut state) = self.shared.state.try_lock() else {
            return None;
        };
        if self.shared.generations.get(channel)?.load(Ordering::Acquire) != generation {
            return None;
        }
        if *state.pending.get(channel)? == Some((key, generation)) {
            return None;
        }
        let tx = worker()?;
        let job = Job { file, key, generation, shared: Arc::clone(&self.shared) };
        if tx.try_send(job).is_ok() {
            *state.pending.get_mut(channel)? = Some((key, generation));
        }
        None
    }
}

fn reserve(columns: usize) -> Option<WaveformTile> {
    let reserved = columns.checked_mul(std::mem::size_of::<Option<(f32, f32)>>())?;
    // Evict globally, not just from the source requesting a replacement. Otherwise old views
    // can pin the entire budget and prevent every later source from ever producing a tile.
    let mut ready = tiles().lock().unwrap_or_else(PoisonError::into_inner);
    while ready.len() >= MAX_TILES || BYTES.load(Ordering::Acquire).saturating_add(reserved) > LIMIT {
        if ready.pop_front().is_none() {
            break;
        }
    }
    BYTES.fetch_update(Ordering::AcqRel, Ordering::Relaxed, |old| old.checked_add(reserved).filter(|n| *n <= LIMIT)).ok()?;
    Some(WaveformTile { spans: Vec::with_capacity(columns), reserved })
}
fn build(job: &Job) -> Option<WaveformTile> {
    let (file, key) = (&job.file, job.key);
    let mut tile = reserve(key.columns)?;
    let (start, step, overlap) = (f64::from_bits(key.start), f64::from_bits(key.step), f64::from_bits(key.overlap));
    let last = file.info().frames.saturating_sub(1);
    let index =
        job.shared.index.lock().unwrap_or_else(PoisonError::into_inner).as_ref().cloned().filter(|index| index.ready() && index.validate().is_ok());
    for col in 0..key.columns {
        if !job.current() {
            return None;
        }
        let a = start + col as f64 * step;
        let b = start + (col + 1) as f64 * step + overlap;
        if file.info().frames == 0 || b < 0.0 || a > last as f64 {
            tile.spans.push(None);
            continue;
        }
        let a = a.max(0.0);
        let b = b.min(last as f64);
        let (first, end) = if key.power {
            // RMS includes the samples just outside the fractional boundaries, matching Peaks.
            (a.floor() as u64, (b.ceil() as u64).saturating_add(1).min(file.info().frames))
        } else {
            // Min/max is the exact line through samples: fractional boundary points plus EVERY
            // integer sample strictly inside/on the window, even when overlap spans many samples.
            (a.ceil() as u64, (b.floor() as u64).saturating_add(1).min(file.info().frames))
        };
        let indexed = index.as_ref().and_then(|index| index.aggregate(key.channel, first, end, || job.current()).ok());
        let aggregate = match indexed {
            Some(aggregate) => aggregate,
            None => raw_aggregate(file, key.channel, first, end, || job.current())?,
        };
        let (mut lo, mut hi, squares, count) = (aggregate.min, aggregate.max, aggregate.squares, aggregate.count);
        if !key.power {
            let interp = |pos: f64| {
                if !job.current() {
                    return None;
                }
                let i = pos.floor() as u64;
                let data = file.read_frames(i, 2).ok()?;
                let ch = data.channels.get(key.channel)?;
                let x = ch.first().copied()?;
                let y = ch.get(1).copied().unwrap_or(x);
                Some(x + (y - x) * (pos - i as f64) as f32)
            };
            let x = interp(a)?;
            let y = interp(b)?;
            lo = lo.min(x).min(y);
            hi = hi.max(x).max(y);
            tile.spans.push(Some((lo, hi)));
        } else {
            tile.spans.push(if count == 0 {
                None
            } else {
                let rms = (squares / count as f64).sqrt() as f32;
                Some((-rms, rms))
            });
        }
    }
    Some(tile)
}

fn raw_aggregate(file: &FileAudio, channel: usize, first: u64, end: u64, current: impl Fn() -> bool) -> Option<disk_index::Aggregate> {
    let mut aggregate = disk_index::Aggregate::EMPTY;
    let mut at = first;
    while at < end {
        if !current() {
            return None;
        }
        let n = usize::try_from((end - at).min(4096)).ok()?;
        let block = file.read_frames(at, n).ok()?;
        let samples = block.channels.get(channel)?;
        if samples.is_empty() {
            return None;
        }
        for &sample in samples {
            aggregate.point(sample);
        }
        at = at.checked_add(samples.len() as u64)?;
        std::thread::yield_now();
    }
    Some(aggregate)
}

mod disk_index {
    use super::*;
    use std::fs::File;
    use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
    use std::sync::atomic::AtomicU8;

    const HEADER: u64 = 64;
    const RECORD: u64 = 24;
    const BASE: u64 = 256;
    const FANOUT: u64 = 4;
    const MAGIC: &[u8; 8] = b"PATPEAK1";
    const IDLE: u8 = 0;
    const QUEUED: u8 = 1;
    const BUILDING: u8 = 2;
    const READY: u8 = 3;
    const FAILED: u8 = 4;

    #[derive(Clone, Copy)]
    pub(super) struct Aggregate {
        pub min: f32,
        pub max: f32,
        pub squares: f64,
        pub count: u64,
    }
    impl Aggregate {
        pub(super) const EMPTY: Self = Self { min: f32::INFINITY, max: f32::NEG_INFINITY, squares: 0.0, count: 0 };
        pub(super) fn point(&mut self, value: f32) {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.squares += f64::from(value) * f64::from(value);
            self.count = self.count.saturating_add(1);
        }
        fn merge(&mut self, other: Self) {
            self.min = self.min.min(other.min);
            self.max = self.max.max(other.max);
            self.squares += other.squares;
            self.count = self.count.saturating_add(other.count);
        }
        fn bytes(self) -> [u8; 24] {
            let mut bytes = [0u8; 24];
            if let Some(out) = bytes.get_mut(0..4) {
                out.copy_from_slice(&self.min.to_le_bytes());
            }
            if let Some(out) = bytes.get_mut(4..8) {
                out.copy_from_slice(&self.max.to_le_bytes());
            }
            if let Some(out) = bytes.get_mut(8..16) {
                out.copy_from_slice(&self.squares.to_le_bytes());
            }
            if let Some(out) = bytes.get_mut(16..24) {
                out.copy_from_slice(&self.count.to_le_bytes());
            }
            bytes
        }
        fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
            let take = |a, b| bytes.get(a..b).ok_or_else(|| "short waveform index record".to_string());
            let min = f32::from_le_bytes(take(0, 4)?.try_into().map_err(|_| "invalid index minimum")?);
            let max = f32::from_le_bytes(take(4, 8)?.try_into().map_err(|_| "invalid index maximum")?);
            let squares = f64::from_le_bytes(take(8, 16)?.try_into().map_err(|_| "invalid index squares")?);
            let count = u64::from_le_bytes(take(16, 24)?.try_into().map_err(|_| "invalid index count")?);
            if count == 0 || !min.is_finite() || !max.is_finite() || min > max || !squares.is_finite() || squares < 0.0 {
                return Err("invalid waveform index aggregate".into());
            }
            Ok(Self { min, max, squares, count })
        }
    }
    #[derive(Debug)]
    enum BuildError {
        Transient,
        Failed(String),
    }
    impl From<String> for BuildError {
        fn from(error: String) -> Self {
            Self::Failed(error)
        }
    }
    impl From<&str> for BuildError {
        fn from(error: &str) -> Self {
            Self::Failed(error.into())
        }
    }
    fn build_error(error: crate::AudioError) -> BuildError {
        match error {
            crate::AudioError::ScratchBusy => BuildError::Transient,
            other => BuildError::Failed(other.to_string()),
        }
    }

    struct Level {
        offset: u64,
        blocks: u64,
        span: u64,
    }
    pub(super) struct Index {
        source: Arc<FileAudio>,
        file: Mutex<File>,
        levels: Vec<Level>,
        length: u64,
        cancelled: Arc<AtomicBool>,
        status: AtomicU8,
    }
    impl Index {
        pub(super) fn new(source: Arc<FileAudio>, output: File, cancelled: Arc<AtomicBool>) -> Result<Self, String> {
            let metadata = output.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.len() != 0 {
                return Err("waveform index must be an exclusively new empty read/write file".into());
            }
            let channels = u64::from(source.info().channels);
            if channels == 0 || channels > MAX_CHANNELS as u64 {
                return Err("invalid waveform index channel count".into());
            }
            let row = channels.checked_mul(RECORD).ok_or("waveform index row overflow")?;
            let mut levels = Vec::with_capacity(32);
            let (mut blocks, mut span, mut offset) = (source.info().frames.div_ceil(BASE), BASE, HEADER);
            while blocks > 0 {
                if levels.len() >= 32 {
                    return Err("too many waveform index levels".into());
                }
                levels.push(Level { offset, blocks, span });
                offset = offset.checked_add(blocks.checked_mul(row).ok_or("waveform index size overflow")?).ok_or("waveform index size overflow")?;
                if blocks == 1 {
                    break;
                }
                blocks = blocks.div_ceil(FANOUT);
                span = span.checked_mul(FANOUT).ok_or("waveform index span overflow")?;
            }
            Ok(Self { source, file: Mutex::new(output), levels, length: offset, cancelled, status: AtomicU8::new(IDLE) })
        }
        fn header(&self, complete: bool) -> Vec<u8> {
            let mut bytes = Vec::with_capacity(HEADER as usize);
            bytes.extend_from_slice(MAGIC);
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&self.source.info().sample_rate.to_le_bytes());
            bytes.extend_from_slice(&u32::from(self.source.info().channels).to_le_bytes());
            bytes.extend_from_slice(&(BASE as u32).to_le_bytes());
            bytes.extend_from_slice(&self.source.info().frames.to_le_bytes());
            bytes.extend_from_slice(&(self.levels.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&u32::from(complete).to_le_bytes());
            bytes.extend_from_slice(&(FANOUT as u32).to_le_bytes());
            bytes.resize(HEADER as usize, 0);
            bytes
        }
        pub(super) fn ready(&self) -> bool {
            self.status.load(Ordering::Acquire) == READY
        }
        pub(super) fn validate(&self) -> Result<(), String> {
            let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
            let mut header = [0u8; HEADER as usize];
            file.seek(SeekFrom::Start(0)).and_then(|_| file.read_exact(&mut header)).map_err(|error| error.to_string())?;
            if header.as_slice() != self.header(true).as_slice() || file.metadata().map_err(|error| error.to_string())?.len() != self.length {
                return Err("waveform index header/source layout mismatch".into());
            }
            Ok(())
        }
        fn active(&self) -> Result<(), String> {
            if self.cancelled.load(Ordering::Acquire) { Err("waveform index source closed".into()) } else { Ok(()) }
        }
        fn build(&self) -> Result<(), BuildError> {
            self.active()?;
            let _scratch = crate::reader::ScratchReservation::new(64 * 1024 + 16 * 1024).map_err(build_error)?;
            let channels = usize::from(self.source.info().channels);
            let row = (channels as u64).checked_mul(RECORD).ok_or("waveform index row overflow")?;
            let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
            file.seek(SeekFrom::Start(0)).and_then(|_| file.write_all(&self.header(false))).map_err(|error| error.to_string())?;
            {
                let mut output = BufWriter::with_capacity(64 * 1024, &mut *file);
                for start in (0..self.source.info().frames).step_by(4096) {
                    self.active()?;
                    let count = usize::try_from((self.source.info().frames - start).min(4096)).map_err(|error| error.to_string())?;
                    let block = self.source.read_frames(start, count).map_err(build_error)?;
                    for first in (0..count).step_by(BASE as usize) {
                        let end = first.saturating_add(BASE as usize).min(count);
                        for data in block.channels.iter().take(channels) {
                            let mut aggregate = Aggregate::EMPTY;
                            for &sample in data.get(first..end).ok_or("short waveform index source block")? {
                                aggregate.point(sample);
                            }
                            output.write_all(&aggregate.bytes()).map_err(|error| error.to_string())?;
                        }
                    }
                    std::thread::yield_now();
                }
                output.flush().map_err(|error| error.to_string())?;
            }
            let mut input = vec![0u8; channels * RECORD as usize * FANOUT as usize];
            let mut output = Vec::with_capacity(channels * RECORD as usize);
            for level in 1..self.levels.len() {
                let child = self.levels.get(level - 1).ok_or("missing child waveform level")?;
                let parent = self.levels.get(level).ok_or("missing parent waveform level")?;
                for block in 0..parent.blocks {
                    self.active()?;
                    let first = block.checked_mul(FANOUT).ok_or("waveform index block overflow")?;
                    let children = child.blocks.saturating_sub(first).min(FANOUT);
                    let size =
                        usize::try_from(children.checked_mul(row).ok_or("waveform index read overflow")?).map_err(|error| error.to_string())?;
                    let slice = input.get_mut(..size).ok_or("waveform index scratch overflow")?;
                    let at = child
                        .offset
                        .checked_add(first.checked_mul(row).ok_or("waveform index offset overflow")?)
                        .ok_or("waveform index offset overflow")?;
                    file.seek(SeekFrom::Start(at)).and_then(|_| file.read_exact(slice)).map_err(|error| error.to_string())?;
                    output.clear();
                    for channel in 0..channels {
                        let mut aggregate = Aggregate::EMPTY;
                        for child_block in 0..children as usize {
                            let at = (child_block * channels + channel) * RECORD as usize;
                            aggregate.merge(Aggregate::from_bytes(slice.get(at..at + RECORD as usize).ok_or("short waveform level record")?)?);
                        }
                        output.extend_from_slice(&aggregate.bytes());
                    }
                    let at = parent
                        .offset
                        .checked_add(block.checked_mul(row).ok_or("waveform index write overflow")?)
                        .ok_or("waveform index write overflow")?;
                    file.seek(SeekFrom::Start(at)).and_then(|_| file.write_all(&output)).map_err(|error| error.to_string())?;
                    std::thread::yield_now();
                }
            }
            self.active()?;
            file.sync_all().map_err(|error| error.to_string())?;
            file.seek(SeekFrom::Start(0))
                .and_then(|_| file.write_all(&self.header(true)))
                .and_then(|_| file.sync_all())
                .map_err(|error| error.to_string())?;
            if file.metadata().map_err(|error| error.to_string())?.len() != self.length {
                return Err("waveform index length mismatch".into());
            }
            Ok(())
        }
        #[cfg(test)]
        pub(super) fn finish_for_test(&self) -> Result<(), String> {
            self.build().map_err(|error| format!("{error:?}"))?;
            self.status.store(READY, Ordering::Release);
            Ok(())
        }

        pub(super) fn aggregate(&self, channel: usize, first: u64, end: u64, current: impl Fn() -> bool) -> Result<Aggregate, String> {
            if channel >= usize::from(self.source.info().channels) || end > self.source.info().frames {
                return Err("waveform index range outside source".into());
            }
            let mut total = Aggregate::EMPTY;
            let mut at = first;
            while at < end {
                self.active()?;
                if !current() {
                    return Err("obsolete waveform query".into());
                }
                let candidate = self.levels.iter().rev().find(|level| at.is_multiple_of(level.span) && level.span <= end - at);
                if let Some(level) = candidate {
                    let block = at / level.span;
                    if block >= level.blocks {
                        return Err("waveform index block outside level".into());
                    }
                    let row = u64::from(self.source.info().channels).checked_mul(RECORD).ok_or("waveform index row overflow")?;
                    let offset = level
                        .offset
                        .checked_add(block.checked_mul(row).ok_or("waveform index record overflow")?)
                        .and_then(|n| n.checked_add((channel as u64) * RECORD))
                        .ok_or("waveform index record overflow")?;
                    let mut data = [0u8; RECORD as usize];
                    let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
                    file.seek(SeekFrom::Start(offset)).and_then(|_| file.read_exact(&mut data)).map_err(|error| error.to_string())?;
                    let record = Aggregate::from_bytes(&data)?;
                    if record.count != level.span {
                        return Err("waveform index span/count mismatch".into());
                    }
                    total.merge(record);
                    at = at.checked_add(level.span).ok_or("waveform index range overflow")?;
                } else {
                    let count = (BASE - at % BASE).min(end - at);
                    let raw = raw_aggregate(&self.source, channel, at, at + count, &current).ok_or("waveform edge read failed")?;
                    total.merge(raw);
                    at = at.checked_add(count).ok_or("waveform edge range overflow")?;
                }
            }
            Ok(total)
        }
    }
    pub(super) fn schedule(index: &Arc<Index>) {
        if index.status.compare_exchange(IDLE, QUEUED, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        static WORKER: OnceLock<Option<SyncSender<Arc<Index>>>> = OnceLock::new();
        let worker = WORKER.get_or_init(|| {
            let (tx, rx) = sync_channel::<Arc<Index>>(8);
            std::thread::Builder::new()
                .name("audio-peak-index".into())
                .spawn(move || {
                    while let Ok(index) = rx.recv() {
                        index.status.store(BUILDING, Ordering::Release);
                        let state = match index.build() {
                            Ok(()) => READY,
                            // Retain partial bytes; never restart an index over the same file.
                            Err(BuildError::Transient) => FAILED,
                            Err(BuildError::Failed(error)) => {
                                log::debug!("retained incomplete waveform index: {error}");
                                FAILED
                            }
                        };
                        index.status.store(state, Ordering::Release);
                    }
                })
                .ok()
                .map(|_| tx)
        });
        if worker.as_ref().is_none_or(|worker| worker.try_send(Arc::clone(index)).is_err()) {
            index.status.store(IDLE, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    // Every fixture is new and retained. No temporary-file auto-cleanup wrapper is used.
    fn file(samples: &[f32]) -> Arc<FileAudio> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("pa-disk-waveform-{}-{stamp}-{}.wav", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let audio = crate::AudioBuffer { sample_rate: 48_000, channels: vec![samples.to_vec()] };
        let bytes = crate::encode(
            &audio,
            &crate::EncodeOptions { format: crate::FileFormat::Wav, bit_depth: crate::BitDepth::Float32, dither: false, bwf: None },
        )
        .unwrap();
        let mut handle = File::create_new(&path).unwrap();
        handle.write_all(&bytes).unwrap();
        drop(handle);
        FileAudio::open(File::open(path).unwrap()).unwrap()
    }

    #[test]
    fn persisted_multiresolution_index_matches_raw_peaks_and_is_lazy_at_attach() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let samples: Vec<f32> = (0..10_257).map(|i| (i as f32 * 0.071).sin() * 0.75).collect();
        let source = file(&samples);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("pa-peak-index-{}-{stamp}-{}.bin", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let output = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
        let retained = output.try_clone().unwrap();
        let cache = WaveformCache::default();
        cache.attach_index(Arc::clone(&source), output).unwrap();
        assert_eq!(retained.metadata().unwrap().len(), 0, "attaching must not start a source scan");
        let index = cache.shared.index.lock().unwrap().as_ref().cloned().unwrap();
        index.finish_for_test().unwrap();
        index.validate().unwrap();
        let summary = index.aggregate(0, 256, 1024, || true).unwrap();
        assert_eq!(summary.count, 768);
        assert_eq!(summary.min, samples[256..1024].iter().copied().fold(f32::INFINITY, f32::min));
        assert_eq!(summary.max, samples[256..1024].iter().copied().fold(f32::NEG_INFINITY, f32::max));
        let peaks = crate::peaks::Peaks::build(&samples);
        for power in [false, true] {
            let key = Key { channel: 0, start: 17.25f64.to_bits(), step: 513.5f64.to_bits(), overlap: 32.25f64.to_bits(), columns: 20, power };
            let job = Job { file: Arc::clone(&source), key, generation: 0, shared: Arc::clone(&cache.shared) };
            let got = build(&job).unwrap().spans.clone();
            if power {
                let expected = peaks.rms_spans(&samples, 17.25, 513.5, 32.25, 20);
                for (got, expected) in got.iter().zip(expected) {
                    match (got, expected) {
                        (Some((_, hi)), Some(expected)) => assert!((*hi - expected).abs() < 1e-4),
                        (None, None) => {}
                        other => panic!("different waveform availability: {other:?}"),
                    }
                }
            } else {
                assert_eq!(got, peaks.spans(&samples, 17.25, 513.5, 32.25, 20));
            }
        }
        assert!(retained.metadata().unwrap().len() > 64);
        assert!(path.is_file(), "persisted index remains available for inspection");
    }

    #[test]
    fn fractional_overlap_retains_every_interior_peak() {
        let file = file(&[0.0, 0.0, 1.0, -1.0, 0.0, 0.0]);
        let shared = Arc::new(Shared::default());
        let key = Key { channel: 0, start: 0.25f64.to_bits(), step: 0.5f64.to_bits(), overlap: 3.0f64.to_bits(), columns: 1, power: false };
        let job = Job { file, key, generation: 0, shared };
        assert_eq!(build(&job).unwrap().spans, vec![Some((-1.0, 1.0))]);
    }

    #[test]
    fn disk_spans_match_resident_peaks_across_fractional_windows() {
        let samples: Vec<f32> = (0..64).map(|i| (i as f32 * 0.31).sin()).collect();
        let file = file(&samples);
        let peaks = crate::peaks::Peaks::build(&samples);
        let shared = Arc::new(Shared::default());
        for start in [-1.0f64, 0.25] {
            for step in [0.25f64, 0.5, 1.5, 7.25] {
                for overlap in [0.0f64, 4.5] {
                    let key = Key { channel: 0, start: start.to_bits(), step: step.to_bits(), overlap: overlap.to_bits(), columns: 8, power: false };
                    let job = Job { file: Arc::clone(&file), key, generation: 0, shared: Arc::clone(&shared) };
                    assert_eq!(
                        build(&job).unwrap().spans,
                        peaks.spans(&samples, start, step, overlap, 8),
                        "start={start}, step={step}, overlap={overlap}"
                    );
                }
            }
        }
    }

    #[test]
    fn dropping_source_cache_cancels_its_outstanding_job() {
        let cache = WaveformCache::default();
        let shared = Arc::clone(&cache.shared);
        let file = file(&[0.0, 1.0]);
        let key = Key { channel: 0, start: 0f64.to_bits(), step: 1f64.to_bits(), overlap: 0f64.to_bits(), columns: 1, power: false };
        let job = Job { file, key, generation: 0, shared };
        assert!(job.current());
        drop(cache);
        assert!(!job.current());
        assert!(build(&job).is_none());
    }
}
