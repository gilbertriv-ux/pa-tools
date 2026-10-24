//! Audio input capture for recording.
//!
//! A [`Recorder`] opens the default input device. While armed, the input callback copies each
//! block into a preallocated lock-free ring (it never locks or allocates), and a writer thread
//! streams the ring into one WAV file per take, rewriting the header every second so a crash leaves
//! a playable file.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use soundcraft_model::TrackId;

/// What [`Recorder::finish`] returns: the takes that hold audio and the first problem met.
pub type Takes = (Vec<(TrackId, PathBuf)>, Option<String>);

/// Frames each block of a [`LiveTake`] sums up.
pub const LIVE_BLOCK: usize = 128;
/// Blocks a live take keeps per channel at most (three hours at 48 kHz); its file records on.
const LIVE_BLOCKS: usize = 8192;

/// A take while it records, summed up for drawing: per channel, the (min, max) of each
/// [`LIVE_BLOCK`] frames so far.
#[derive(Debug, Clone)]
pub struct LiveTake {
    pub channels: Vec<Vec<(f32, f32)>>,
    pub block_frames: usize,
    max_blocks: usize,
}

const RING_SECONDS: usize = 4;
/// How long a recording input may go without a callback before it counts as gone.
const STALL: Duration = Duration::from_secs(2);
const GONE: u8 = 1;
const BROKEN: u8 = 2;

/// Why the input can't go on.
#[derive(Debug, Clone, PartialEq)]
pub enum Trouble {
    /// The device went away or stopped sending audio, or its callback hit a bug.
    Device(String),
    /// Writing a take failed (a full disk, say).
    Disk(String),
}

/// Single-producer single-consumer FIFO of samples (input callback → writer thread). The slots
/// are atomics, so it needs no lock and no `unsafe`.
struct CaptureRing {
    slots: Box<[AtomicU32]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl CaptureRing {
    fn new(capacity: usize) -> CaptureRing {
        let n = capacity.max(2).next_power_of_two();
        CaptureRing { slots: (0..n).map(|_| AtomicU32::new(0)).collect(), mask: n - 1, head: AtomicUsize::new(0), tail: AtomicUsize::new(0) }
    }

    /// Publish whole interleaved frames only; unused slots never split a channel frame.
    fn push_aligned(&self, n: usize, channels: usize, sample: impl Fn(usize) -> f32) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let channels = channels.max(1);
        let room = self.slots.len().saturating_sub(head.wrapping_sub(self.tail.load(Ordering::Acquire)));
        let n = n.min(room) / channels * channels;
        for i in 0..n {
            if let Some(slot) = self.slots.get(head.wrapping_add(i) & self.mask) {
                slot.store(sample(i).to_bits(), Ordering::Relaxed);
            }
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    fn pop_all(&self, out: &mut Vec<f32>) {
        let (tail, head) = (self.tail.load(Ordering::Relaxed), self.head.load(Ordering::Acquire));
        out.extend(
            (0..head.wrapping_sub(tail))
                .map(|i| self.slots.get(tail.wrapping_add(i) & self.mask).map_or(0.0, |s| f32::from_bits(s.load(Ordering::Relaxed)))),
        );
        self.tail.store(head, Ordering::Release);
    }
}

/// An input sample, or silence for NaN and infinities, which would poison the take and the mix.
fn finite(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

/// A callback owns this stack guard; unwinding and every ordinary return release it.
struct CallbackExit<'a>(&'a AtomicBool);
impl Drop for CallbackExit<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

struct Shared {
    armed: AtomicBool,
    in_callback: AtomicBool,
    /// The newest arm accepts only a later output recording epoch.
    minimum_generation: AtomicU64,
    ring: CaptureRing,
    /// Samples lost while the ring was full.
    dropped: AtomicUsize,
    /// Input callbacks so far, to tell a device that stopped sending.
    beats: AtomicU64,
    /// Set from the input stream when it can't go on: `GONE` or `BROKEN`.
    fault: AtomicU8,
    /// The first failure writing a take, until it's reported.
    failed: Mutex<Option<String>>,
}

impl Shared {
    fn new(ring: CaptureRing) -> Shared {
        let (beats, fault, failed) = (AtomicU64::new(0), AtomicU8::new(0), Mutex::new(None));
        Shared {
            armed: AtomicBool::new(false),
            in_callback: AtomicBool::new(false),
            minimum_generation: AtomicU64::new(0),
            ring,
            dropped: AtomicUsize::new(0),
            beats,
            fault,
            failed,
        }
    }

    fn accepts_master(&self, generation: u64, recording: bool) -> bool {
        recording && generation >= self.minimum_generation.load(Ordering::Acquire)
    }

    /// Run the input callback's `work`. After a panic in it, the input is broken and does
    /// nothing more.
    fn guarded(&self, work: impl FnOnce()) {
        // Never wait on the realtime thread. Unexpected reentrancy fails closed.
        if self.in_callback.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            self.fault.store(BROKEN, Ordering::Relaxed);
            return;
        }
        let _exit = CallbackExit(&self.in_callback);
        self.beats.fetch_add(1, Ordering::Relaxed);
        if self.fault.load(Ordering::Relaxed) != BROKEN && std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
            self.fault.store(BROKEN, Ordering::Relaxed);
        }
    }

    /// Control-thread only. Close capture before waiting for any previously admitted callback.
    /// A newly entering callback sees capture closed until arm publishes its new epoch.
    fn close_capture_and_wait(&self, timeout: Duration) -> Result<(), &'static str> {
        self.armed.store(false, Ordering::SeqCst);
        let started = Instant::now();
        while self.in_callback.load(Ordering::SeqCst) {
            if started.elapsed() >= timeout {
                return Err("audio input callback did not quiesce before arming; capture remains stopped");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    /// The input callback's work while armed. Samples that don't fit are owed (`owed`) and queued
    /// as silence before the next block, so the take keeps its timing.
    #[cfg(test)]
    fn capture(&self, data: &[f32], owed: &mut usize) {
        self.capture_aligned(data, owed, 1);
    }

    fn capture_aligned(&self, data: &[f32], owed: &mut usize, channels: usize) {
        let channels = channels.max(1);
        if data.len() % channels != 0 {
            self.fault.store(BROKEN, Ordering::Relaxed);
            return;
        }
        *owed -= self.ring.push_aligned(*owed, channels, |_| 0.0);
        let kept = if *owed == 0 { self.ring.push_aligned(data.len(), channels, |i| data.get(i).copied().map_or(0.0, finite)) } else { 0 };
        *owed = owed.saturating_add(data.len() - kept);
        self.dropped.fetch_add(data.len() - kept, Ordering::Relaxed);
    }
}

pub struct Recorder {
    shared: Arc<Shared>,
    /// Live input for monitoring (always fed while the input stream runs).
    pub monitor: Arc<InputRing>,
    #[cfg(not(target_os = "freebsd"))]
    _stream: Option<cpal::Stream>,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: usize,
    /// Requested buffer after fallback; not a measured hardware buffer size.
    pub device_buffer: Option<u32>,
    /// Recording follows the output callback clock; monitoring retains native-rate SRC.
    master_clock: Option<Arc<crate::duplex::MasterClock>>,
    /// The take writer, and what each track's take holds so far.
    writer: Option<(std::thread::JoinHandle<Takes>, Vec<(TrackId, Arc<Mutex<LiveTake>>)>)>,
    /// The callback count last seen, and since when.
    seen: (u64, Instant),
}

/// The input stream: device samples are converted to f32 in a scratch buffer allocated here (so
/// the callback never allocates), then feed the monitor ring and, while armed, the capture.
#[cfg(not(target_os = "freebsd"))]
fn build_input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: &Arc<Shared>,
    monitor: &Arc<InputRing>,
    master_clock: Option<Arc<crate::duplex::MasterClock>>,
    output_rate: u32,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::Sample;
    use cpal::traits::DeviceTrait;
    let (sh, errors) = (Arc::clone(shared), Arc::clone(shared));
    let mon = Arc::clone(monitor);
    let channels = usize::from(config.channels);
    let native_rate = config.sample_rate.0;
    let channel_limit = if master_clock.is_some() { 64 } else { 256 };
    if channels == 0 || channels > channel_limit || native_rate == 0 {
        return Err(cpal::BuildStreamError::BackendSpecific {
            err: cpal::BackendSpecificError { description: "unsupported bounded input format".into() },
        });
    }
    let mut scratch = vec![0.0f32; 8192 * channels];
    let chunk = scratch.len();
    let mut sync = if master_clock.is_some() {
        Some(
            crate::duplex::TimestampResampler::new(channels, native_rate, output_rate)
                .map_err(|why| cpal::BuildStreamError::BackendSpecific { err: cpal::BackendSpecificError { description: why.into() } })?,
        )
    } else {
        None
    };
    let mut sync_scratch = vec![0.0f32; chunk];
    let mut owed = 0;
    device.build_input_stream(
        config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            crate::mark_audio_thread();
            let timestamp = info.timestamp();
            mon.timing.observe(timestamp.callback.duration_since(&timestamp.capture));
            sh.guarded(|| {
                let capture_ns =
                    timestamp.capture.duration_since(&cpal::StreamInstant::new(0, 0)).and_then(|duration| u64::try_from(duration.as_nanos()).ok());
                if sync.is_some() && capture_ns.is_none() {
                    sh.fault.store(BROKEN, Ordering::Relaxed);
                    return;
                }
                for (part_index, part) in data.chunks(chunk).enumerate() {
                    let Some(s) = scratch.get_mut(..part.len()) else { return };
                    for (o, x) in s.iter_mut().zip(part) {
                        *o = f32::from_sample(*x);
                    }
                    // Native-rate monitoring is deliberately distinct from take-clock alignment.
                    mon.push(s);
                    if sh.armed.load(Ordering::SeqCst) {
                        if let (Some(sync), Some(master)) = (&mut sync, &master_clock) {
                            let Some(clock) = master.snapshot() else { continue };
                            if !sh.accepts_master(clock.generation, clock.recording) {
                                owed = 0;
                                continue;
                            }
                            let frame_offset = part_index.saturating_mul(chunk / channels);
                            let offset_ns = (frame_offset as u128).saturating_mul(1_000_000_000) / u128::from(native_rate.max(1));
                            let Some(start_ns) = capture_ns.and_then(|ns| u64::try_from(offset_ns).ok().and_then(|offset| ns.checked_add(offset)))
                            else {
                                sh.fault.store(BROKEN, Ordering::Relaxed);
                                return;
                            };
                            let mut used = 0usize;
                            let result = sync.process(s, start_ns, clock, |frame| {
                                if used.saturating_add(frame.len()) > sync_scratch.len() {
                                    sh.capture_aligned(&sync_scratch[..used], &mut owed, channels);
                                    used = 0;
                                }
                                if let Some(to) = sync_scratch.get_mut(used..used.saturating_add(frame.len())) {
                                    to.copy_from_slice(frame);
                                    used += frame.len();
                                }
                            });
                            if used != 0 {
                                sh.capture_aligned(&sync_scratch[..used], &mut owed, channels);
                            }
                            if result.is_err() {
                                sh.fault.store(BROKEN, Ordering::Relaxed);
                                return;
                            }
                        } else {
                            sh.capture_aligned(s, &mut owed, channels);
                        }
                    } else {
                        owed = 0;
                    }
                }
            });
        },
        move |e| {
            // Possibly the audio thread (see `crate::mark_audio_thread`).
            crate::mark_audio_thread();
            if matches!(e, cpal::StreamError::DeviceNotAvailable) {
                let _ = errors.fault.compare_exchange(0, GONE, Ordering::Relaxed, Ordering::Relaxed);
            }
            log::warn!("input stream error: {e}");
        },
        None,
    )
}

impl Recorder {
    /// Open the default input device. Errors when there is none (the caller shows a message).
    pub fn open() -> Result<Recorder, String> {
        Recorder::open_with(&crate::AudioConfig::default())
    }

    /// Open the input device `audio` chooses (else the default) at its chosen rate and buffer
    /// size when the device offers them, else at the device's defaults. Integer devices (ASIO
    /// drivers usually are) are converted to f32.
    #[cfg(not(target_os = "freebsd"))]
    pub fn open_with(audio: &crate::AudioConfig) -> Result<Recorder, String> {
        Self::open_inner(audio, None)
    }

    /// Align recorded take samples to the output callback's software master clock.
    /// This does not claim a shared physical oscillator or drift-corrected live monitoring.
    #[cfg(not(target_os = "freebsd"))]
    pub fn open_with_master(audio: &crate::AudioConfig, clock: Arc<crate::duplex::MasterClock>, output_rate: u32) -> Result<Recorder, String> {
        if !(8_000..=768_000).contains(&output_rate) {
            return Err("invalid output master sample rate".into());
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (audio, clock);
            return Err("output-master timestamp synchronization is currently verified only for the macOS timestamp domain".into());
        }
        #[cfg(target_os = "macos")]
        Self::open_inner(audio, Some((clock, output_rate)))
    }

    #[cfg(not(target_os = "freebsd"))]
    fn open_inner(audio: &crate::AudioConfig, master: Option<(Arc<crate::duplex::MasterClock>, u32)>) -> Result<Recorder, String> {
        use cpal::traits::{DeviceTrait, StreamTrait};
        let host = crate::devices::host(audio);
        let device = crate::devices::device(&host, audio.input.as_deref(), false).ok_or("no audio input device")?;
        #[allow(deprecated)]
        let name = device.name().unwrap_or_else(|_| "Input".into());
        // At the chosen rate when the device offers it, so input and output run together: the
        // best format first, then the most channels.
        let at_rate = audio.sample_rate.and_then(|r| {
            device
                .supported_input_configs()
                .ok()?
                .filter(|c| crate::devices::format_rank(c.sample_format()).is_some() && c.min_sample_rate().0 <= r && c.max_sample_rate().0 >= r)
                .min_by_key(|c| (crate::devices::format_rank(c.sample_format()), std::cmp::Reverse(c.channels())))
                .map(|c| c.with_sample_rate(cpal::SampleRate(r)))
        });
        let cfg = match at_rate {
            Some(c) => c,
            None => device.default_input_config().map_err(|e| e.to_string())?,
        };
        let format = cfg.sample_format();
        if crate::devices::format_rank(format).is_none() {
            return Err(format!("unsupported input sample format {format:?}"));
        }
        let mut config = cfg.config();
        config.buffer_size = crate::devices::buffer_size(audio.buffer_size, cfg.buffer_size());
        let channels = usize::from(config.channels).max(1);
        let ring = CaptureRing::new((config.sample_rate.0 as usize).saturating_mul(channels * RING_SECONDS).min(1 << 25));
        let shared = Arc::new(Shared::new(ring));
        let monitor = InputRing::new(usize::from(config.channels), config.sample_rate.0);
        let output_rate = master.as_ref().map_or(config.sample_rate.0, |(_, rate)| *rate);
        let master_clock = master.map(|(clock, _)| clock);
        let build = |config: &cpal::StreamConfig| match format {
            cpal::SampleFormat::I32 => build_input::<i32>(&device, config, &shared, &monitor, master_clock.clone(), output_rate),
            cpal::SampleFormat::I16 => build_input::<i16>(&device, config, &shared, &monitor, master_clock.clone(), output_rate),
            _ => build_input::<f32>(&device, config, &shared, &monitor, master_clock.clone(), output_rate),
        };
        // The chosen buffer size first; if the device refuses it, its default.
        let stream = match build(&config) {
            Ok(s) => s,
            Err(e) if config.buffer_size != cpal::BufferSize::Default => {
                log::warn!("cannot open the input with buffer {:?}: {e}; using the device default", config.buffer_size);
                config.buffer_size = cpal::BufferSize::Default;
                build(&config).map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };
        stream.play().map_err(|e| e.to_string())?;
        let seen = (0, Instant::now());
        let device_buffer = match config.buffer_size {
            cpal::BufferSize::Fixed(n) => Some(n),
            cpal::BufferSize::Default => None,
        };
        Ok(Recorder {
            monitor,
            shared,
            _stream: Some(stream),
            device_name: name,
            sample_rate: output_rate,
            channels,
            device_buffer,
            master_clock,
            writer: None,
            seen,
        })
    }

    #[cfg(target_os = "freebsd")]
    pub fn open_with(_audio: &crate::AudioConfig) -> Result<Recorder, String> {
        Err("recording is not supported on this platform yet".into())
    }

    #[cfg(target_os = "freebsd")]
    pub fn open_with_master(_audio: &crate::AudioConfig, _clock: Arc<crate::duplex::MasterClock>, _output_rate: u32) -> Result<Recorder, String> {
        Err("recording is not supported on this platform yet".into())
    }

    pub fn clock_relationship(&self) -> crate::duplex::ClockRelationship {
        if self.master_clock.is_some() { crate::duplex::ClockRelationship::SoftwareMaster } else { crate::duplex::ClockRelationship::Independent }
    }

    /// Capture-to-callback timestamp latency, or a labelled requested-buffer estimate.
    pub fn input_latency(&self) -> Option<crate::latency::StreamLatency> {
        self.monitor.timing.snapshot(self.device_buffer, self.monitor.rate)
    }

    /// Create each take's file and start recording into them: a track's take records the given
    /// input channels into a new file.
    pub fn arm(&mut self, takes: Vec<(TrackId, Vec<usize>, PathBuf)>) -> Result<(), String> {
        if self.writer.is_some() {
            return Err("already recording".into());
        }
        if takes.iter().any(|(_, inputs, _)| inputs.is_empty() || inputs.len() > self.channels || inputs.iter().any(|i| *i >= self.channels)) {
            return Err("a take must select valid input channels".into());
        }
        // No take path is created until old input work is outside the capture critical section.
        self.shared.close_capture_and_wait(Duration::from_millis(250)).map_err(str::to_owned)?;
        let mut takes =
            takes.into_iter().map(|(id, inputs, path)| TakeWriter::create(id, inputs, path, self.sample_rate)).collect::<Result<Vec<_>, _>>()?;
        let total_channels = takes.iter().map(|take| take.inputs.len()).sum::<usize>().max(1);
        let bound = (8 * 1024 * 1024 / 8 / total_channels).clamp(2, LIVE_BLOCKS);
        let bound = 1usize << (usize::BITS - 1 - bound.leading_zeros());
        for take in &mut takes {
            take.live.lock().unwrap_or_else(PoisonError::into_inner).max_blocks = bound;
        }
        let live = takes.iter().map(|t| (t.id, Arc::clone(&t.live))).collect();
        let ring = &self.shared.ring;
        ring.tail.store(ring.head.load(Ordering::Acquire), Ordering::Release);
        *self.shared.failed.lock().unwrap_or_else(PoisonError::into_inner) = None;
        // Read the output epoch directly: a contended snapshot must never admit an old take.
        let minimum_generation = self.master_clock.as_ref().map_or(0, |clock| clock.next_generation());
        self.shared.minimum_generation.store(minimum_generation, Ordering::Release);
        self.shared.armed.store(true, Ordering::SeqCst);
        let (shared, channels) = (Arc::clone(&self.shared), self.channels);
        match std::thread::Builder::new().name("take writer".into()).spawn(move || write_takes(&shared, takes, channels)) {
            Ok(w) => self.writer = Some((w, live)),
            Err(e) => {
                self.shared.armed.store(false, Ordering::SeqCst);
                return Err(e.to_string());
            }
        }
        Ok(())
    }

    /// What the take recording onto `track` holds so far.
    pub fn live_take(&self, track: TrackId) -> Option<MutexGuard<'_, LiveTake>> {
        let (_, take) = self.writer.as_ref()?.1.iter().find(|(id, _)| *id == track)?;
        Some(take.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Why the input can't go on, if it can't: the device went away or, while recording, stopped
    /// sending audio; its callback hit a bug; or writing a take failed (reported once).
    pub fn trouble(&mut self) -> Option<Trouble> {
        if let Some(e) = self.shared.failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
            return Some(Trouble::Disk(e));
        }
        let name = &self.device_name;
        match self.shared.fault.load(Ordering::Relaxed) {
            GONE => return Some(Trouble::Device(format!("“{name}” was disconnected"))),
            BROKEN => return Some(Trouble::Device("the audio input hit an internal error (see the log)".into())),
            _ => {}
        }
        let beats = self.shared.beats.load(Ordering::Relaxed);
        if beats != self.seen.0 || !self.shared.armed.load(Ordering::Relaxed) {
            self.seen = (beats, Instant::now());
        }
        (self.seen.1.elapsed() >= STALL).then(|| Trouble::Device(format!("“{name}” stopped sending audio")))
    }

    /// Input the current take has lost to a slow disk so far, in milliseconds.
    pub fn lost_ms(&self) -> u64 {
        let frames = self.shared.dropped.load(Ordering::Relaxed) / self.channels.max(1);
        (frames as u64).saturating_mul(1000) / u64::from(self.sample_rate.max(1))
    }

    pub fn finish(&mut self) -> Takes {
        self.shared.armed.store(false, Ordering::SeqCst);
        let (takes, problem) = self.writer.take().and_then(|(w, _)| w.join().ok()).unwrap_or_default();
        let lost = self.shared.dropped.swap(0, Ordering::Relaxed) / self.channels.max(1);
        let slow =
            (lost > 0).then(|| format!("{} ms of input lost: the disk was too slow", lost.saturating_mul(1000) / self.sample_rate.max(1) as usize));
        (takes, problem.or(slow))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish();
    }
}

fn write_takes(shared: &Shared, mut takes: Vec<TakeWriter>, channels: usize) -> Takes {
    let (mut block, mut header_written, mut reported) = (Vec::new(), Instant::now(), false);
    loop {
        let stopping = !shared.armed.load(Ordering::SeqCst) && !shared.in_callback.load(Ordering::SeqCst);
        // The ring holds interleaved samples and a pop can end mid-frame: write whole frames and
        // keep the remainder for the next pop, so channels never shift.
        shared.ring.pop_all(&mut block);
        let whole = block.len() - block.len() % channels.max(1);
        takes.iter_mut().for_each(|t| t.write(&block[..whole], channels));
        block.drain(..whole);
        if !reported && let Some(e) = takes.iter().find_map(|t| t.error.clone()) {
            *shared.failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
            reported = true;
        }
        if stopping {
            break;
        }
        if header_written.elapsed() >= Duration::from_secs(1) {
            header_written = Instant::now();
            takes.iter_mut().for_each(TakeWriter::write_header);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let (mut done, mut problem) = (Vec::new(), None);
    for mut t in takes {
        t.write_header();
        // Preserve even empty/failed take files; a recording never authorizes deletion.
        if t.frames != 0 {
            done.push((t.id, t.path));
        }
        problem = problem.or(t.error);
    }
    (done, problem)
}

/// One take's file. After an error it stops writing; its header keeps covering what was written.
struct TakeWriter {
    id: TrackId,
    inputs: Vec<usize>,
    path: PathBuf,
    file: File,
    bytes: Vec<u8>,
    rate: u32,
    frames: u64,
    error: Option<String>,
    live: Arc<Mutex<LiveTake>>,
}

impl TakeWriter {
    fn create(id: TrackId, inputs: Vec<usize>, path: PathBuf, rate: u32) -> Result<TakeWriter, String> {
        if inputs.is_empty() || inputs.len() > usize::from(u16::MAX) || rate == 0 {
            return Err("invalid take channel count or sample rate".into());
        }
        let file = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| File::create_new(&path));
        let file = file.map_err(|e| format!("{}: {e}", path.display()))?;
        let live = Arc::new(Mutex::new(LiveTake { channels: vec![Vec::new(); inputs.len()], block_frames: LIVE_BLOCK, max_blocks: LIVE_BLOCKS }));
        let mut take = TakeWriter { id, inputs, path, file, bytes: Vec::with_capacity(64 * 1024), rate, frames: 0, error: None, live };
        take.write_header();
        take.error.take().map_or(Ok(take), Err)
    }

    /// Append this take's channels of `block` (interleaved, `channels` wide).
    fn write(&mut self, block: &[f32], channels: usize) {
        let channels = channels.max(1);
        if self.error.is_some() {
            return;
        }
        // Bounded conversion scratch, reused across writes; never allocate a take-sized buffer.
        let frames_per_chunk = (64 * 1024 / self.inputs.len().max(1) / 4).max(1);
        let samples_per_chunk = frames_per_chunk.saturating_mul(channels);
        for part in block.chunks(samples_per_chunk) {
            self.bytes.clear();
            for frame in part.chunks_exact(channels) {
                for &input in &self.inputs {
                    self.bytes.extend_from_slice(&frame.get(input).copied().unwrap_or(0.0).to_le_bytes());
                }
            }
            match self.file.write_all(&self.bytes) {
                Ok(()) => {
                    self.summarize(part, channels);
                    self.frames = self.frames.saturating_add((part.len() / channels) as u64);
                }
                Err(e) => {
                    self.fail(e);
                    break;
                }
            }
        }
    }

    /// Add this take's channels of `block` to its live take.
    fn summarize(&self, block: &[f32], channels: usize) {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        for (f, n) in block.chunks_exact(channels).zip(self.frames..) {
            while n / (live.block_frames as u64) >= live.max_blocks as u64 {
                for blocks in &mut live.channels {
                    let pairs = blocks.len().div_ceil(2);
                    for index in 0..pairs {
                        let first = blocks.get(index * 2).copied().unwrap_or((0.0, 0.0));
                        let second = blocks.get(index * 2 + 1).copied().unwrap_or(first);
                        if let Some(to) = blocks.get_mut(index) {
                            *to = (first.0.min(second.0), first.1.max(second.1));
                        }
                    }
                    blocks.truncate(pairs); // In-memory vector only; no file operation.
                }
                live.block_frames = live.block_frames.saturating_mul(2);
            }
            let at = (n / live.block_frames as u64) as usize;
            let max_blocks = live.max_blocks;
            for (blocks, &i) in live.channels.iter_mut().zip(&self.inputs) {
                let v = f.get(i).copied().unwrap_or(0.0);
                match blocks.get_mut(at) {
                    Some(b) => *b = (b.0.min(v), b.1.max(v)),
                    None if at < max_blocks => blocks.push((v, v)),
                    None => {}
                }
            }
        }
    }

    fn write_header(&mut self) {
        let header = match soundcraft_audio_io::wav_float_header(self.inputs.len(), self.rate, self.frames) {
            Ok(h) => h,
            Err(e) => return self.fail(e),
        };
        let f = &mut self.file;
        if let Err(e) = f.seek(SeekFrom::Start(0)).and_then(|_| f.write_all(&header)).and_then(|()| f.seek(SeekFrom::End(0))) {
            self.fail(e);
        }
    }

    fn fail(&mut self, e: impl std::fmt::Display) {
        self.error.get_or_insert_with(|| format!("{}: {e}", self.path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(path: &PathBuf) -> Vec<Vec<f32>> {
        soundcraft_audio_io::decode(&std::fs::read(path).unwrap(), None).unwrap().1.channels
    }

    #[test]
    fn a_full_ring_owes_the_lost_samples_as_silence() {
        let shared = Shared::new(CaptureRing::new(8));
        let (mut owed, mut out) = (0, Vec::new());
        shared.capture(&[1.0; 6], &mut owed);
        shared.capture(&[2.0; 4], &mut owed);
        shared.ring.pop_all(&mut out);
        shared.capture(&[3.0; 2], &mut owed);
        shared.ring.pop_all(&mut out);
        assert_eq!(out, [vec![1.0; 6], vec![2.0; 2], vec![0.0; 2], vec![3.0; 2]].concat());
        assert_eq!((owed, shared.dropped.load(Ordering::Relaxed)), (0, 2));
    }

    #[test]
    fn control_quiescence_waits_for_inflight_callback_before_rearming() {
        let shared = Arc::new(Shared::new(CaptureRing::new(16)));
        shared.armed.store(true, Ordering::SeqCst);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let callback_shared = Arc::clone(&shared);
        let callback = std::thread::spawn(move || {
            callback_shared.guarded(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let control_shared = Arc::clone(&shared);
        let control = std::thread::spawn(move || {
            done_tx.send(control_shared.close_capture_and_wait(Duration::from_secs(1))).unwrap();
        });
        let started = Instant::now();
        while shared.armed.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(1) {
            std::thread::yield_now();
        }
        assert!(!shared.armed.load(Ordering::SeqCst));
        assert!(matches!(done_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        release_tx.send(()).unwrap();
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
        callback.join().unwrap();
        control.join().unwrap();
        assert!(!shared.in_callback.load(Ordering::SeqCst));
    }

    #[test]
    fn callback_unwind_releases_quiescence_flag_and_reentrancy_never_waits() {
        let shared = Shared::new(CaptureRing::new(16));
        shared.guarded(|| panic!("simulated callback failure"));
        assert!(!shared.in_callback.load(Ordering::SeqCst));
        assert_eq!(shared.fault.load(Ordering::Relaxed), BROKEN);
        assert_eq!(shared.close_capture_and_wait(Duration::ZERO), Ok(()));
        let reentrant = Shared::new(CaptureRing::new(16));
        reentrant.guarded(|| reentrant.guarded(|| panic!("reentrant work must not run")));
        assert!(!reentrant.in_callback.load(Ordering::SeqCst));
        assert_eq!(reentrant.fault.load(Ordering::Relaxed), BROKEN);
    }

    #[test]
    fn control_quiescence_timeout_leaves_capture_closed() {
        let shared = Shared::new(CaptureRing::new(16));
        shared.armed.store(true, Ordering::SeqCst);
        shared.in_callback.store(true, Ordering::SeqCst);
        assert!(shared.close_capture_and_wait(Duration::ZERO).is_err());
        assert!(!shared.armed.load(Ordering::SeqCst));
        shared.in_callback.store(false, Ordering::SeqCst);
        assert_eq!(shared.close_capture_and_wait(Duration::ZERO), Ok(()));
    }

    #[test]
    fn newly_armed_take_rejects_the_previous_active_output_epoch() {
        let master = crate::duplex::MasterClock::new();
        master.publish(1_000_000_000, 64, 48_000, true);
        let previous = master.snapshot().unwrap();
        let shared = Shared::new(CaptureRing::new(16));
        shared.minimum_generation.store(master.next_generation(), Ordering::Release);
        assert!(!shared.accepts_master(previous.generation, previous.recording));
        master.invalidate();
        master.publish(1_010_000_000, 64, 48_000, true);
        let fresh = master.snapshot().unwrap();
        assert!(shared.accepts_master(fresh.generation, fresh.recording));
        assert!(!shared.accepts_master(fresh.generation, false));
    }

    #[test]
    fn capture_overflow_preserves_three_channel_frame_boundaries() {
        let shared = Shared::new(CaptureRing::new(8));
        let mut owed = 0;
        let mut out = Vec::new();
        shared.capture_aligned(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], &mut owed, 3);
        shared.capture_aligned(&[7.0, 8.0, 9.0], &mut owed, 3);
        shared.ring.pop_all(&mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(owed, 3);
        out.clear();
        shared.capture_aligned(&[10.0, 11.0, 12.0], &mut owed, 3);
        shared.ring.pop_all(&mut out);
        assert_eq!(out, [0.0, 0.0, 0.0, 10.0, 11.0, 12.0]);
        assert_eq!(owed, 0);
    }

    #[test]
    fn nan_and_infinite_input_records_as_silence() {
        let shared = Shared::new(CaptureRing::new(8));
        let (mut owed, mut out) = (0, Vec::new());
        shared.capture(&[0.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.5], &mut owed);
        shared.ring.pop_all(&mut out);
        assert_eq!(out, [0.5, 0.0, 0.0, 0.0, -0.5]);
    }

    fn recorder(shared: &Arc<Shared>) -> Recorder {
        Recorder {
            shared: Arc::clone(shared),
            monitor: InputRing::new(1, 48_000),
            #[cfg(not(target_os = "freebsd"))]
            _stream: None,
            device_name: "Mic In".into(),
            sample_rate: 48_000,
            channels: 1,
            device_buffer: None,
            master_clock: None,
            writer: None,
            seen: (0, Instant::now()),
        }
    }

    #[test]
    fn a_bug_a_stall_or_a_lost_device_is_trouble() {
        let shared = Arc::new(Shared::new(CaptureRing::new(64)));
        let mut r = recorder(&shared);
        shared.armed.store(true, Ordering::Release);
        assert_eq!(r.trouble(), None);
        r.seen.1 = Instant::now().checked_sub(STALL).unwrap();
        assert_eq!(r.trouble(), Some(Trouble::Device("“Mic In” stopped sending audio".into())));
        shared.guarded(|| {});
        assert_eq!(r.trouble(), None, "it sent again");
        shared.armed.store(false, Ordering::Release);
        r.seen.1 = Instant::now().checked_sub(STALL).unwrap();
        assert_eq!(r.trouble(), None, "a quiet input that isn't recording is fine");
        shared.guarded(|| std::panic::panic_any("a bug in the input"));
        let mut ran = false;
        shared.guarded(|| ran = true);
        assert!(!ran);
        assert_eq!(r.trouble(), Some(Trouble::Device("the audio input hit an internal error (see the log)".into())));
        let gone = Arc::new(Shared::new(CaptureRing::new(64)));
        gone.fault.store(GONE, Ordering::Relaxed);
        assert_eq!(recorder(&gone).trouble(), Some(Trouble::Device("“Mic In” was disconnected".into())));
    }

    // Preserve every test artifact. Atomic create_dir reserves a fresh path without cleanup.
    fn test_dir(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..1000 {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("pa-m1-{label}-{}-{now}-{n}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot reserve test directory: {e}"),
            }
        }
        panic!("cannot reserve a unique test directory")
    }

    #[test]
    fn empty_take_is_retained_and_invalid_input_is_rejected_before_creation() {
        let dir = test_dir("empty-preserved");
        let path = dir.join("empty.wav");
        let take = TakeWriter::create(TrackId(1), vec![0], path.clone(), 48_000).unwrap();
        let shared = Shared::new(CaptureRing::new(64));
        let (takes, problem) = write_takes(&shared, vec![take], 1);
        assert!(takes.is_empty());
        assert!(problem.is_none());
        assert!(path.is_file());
        assert!(decode(&path).iter().all(Vec::is_empty));
        let invalid = dir.join("invalid.wav");
        assert!(TakeWriter::create(TrackId(2), vec![], invalid.clone(), 48_000).is_err());
        assert!(!invalid.exists());
    }

    #[test]
    fn live_waveform_coarsens_instead_of_growing_with_take_duration() {
        let dir = test_dir("bounded-live");
        let mut take = TakeWriter::create(TrackId(1), vec![0], dir.join("take.wav"), 48_000).unwrap();
        take.live.lock().unwrap().max_blocks = 2;
        take.write(&vec![0.25; LIVE_BLOCK * 16], 1);
        let live = take.live.lock().unwrap();
        assert!(live.channels[0].len() <= 2);
        assert!(live.block_frames >= LIVE_BLOCK * 8);
        assert!(live.channels[0].iter().all(|range| *range == (0.25, 0.25)));
    }

    #[test]
    fn disk_conversion_scratch_is_reused_for_large_blocks() {
        let dir = test_dir("bounded-writer");
        let path = dir.join("long.wav");
        let mut take = TakeWriter::create(TrackId(1), vec![0, 1], path.clone(), 48_000).unwrap();
        let capacity = take.bytes.capacity();
        take.write(&vec![0.25; 200_000], 2);
        assert_eq!(take.bytes.capacity(), capacity);
        assert_eq!(take.frames, 100_000);
        take.write_header();
        assert_eq!(decode(&path)[0].len(), 100_000);
    }

    #[test]
    fn a_failed_write_is_trouble_while_still_recording() {
        let dir = test_dir("failed-take");
        let mut take = TakeWriter::create(TrackId(1), vec![0], dir.join("Vox_01.wav"), 48_000).unwrap();
        take.file = File::open(&take.path).unwrap();
        let shared = Arc::new(Shared::new(CaptureRing::new(1 << 12)));
        shared.armed.store(true, Ordering::Release);
        let mut r = recorder(&shared);
        let sh = Arc::clone(&shared);
        let writer = std::thread::spawn(move || write_takes(&sh, vec![take], 1));
        shared.capture(&[0.5; 480], &mut 0);
        let trouble = (0..300).find_map(|_| {
            std::thread::sleep(Duration::from_millis(10));
            r.trouble()
        });
        assert!(matches!(&trouble, Some(Trouble::Disk(e)) if e.contains("Vox_01.wav")), "{trouble:?}");
        assert_eq!(r.trouble(), None, "reported once");
        shared.armed.store(false, Ordering::Release);
        assert!(writer.join().unwrap().1.is_some());
    }

    #[test]
    fn takes_stream_into_their_files() {
        let dir = test_dir("takes");
        let gtr = TakeWriter::create(TrackId(1), vec![1, 2], dir.join("Gtr_01.wav"), 48_000).unwrap();
        let mut vox = TakeWriter::create(TrackId(2), vec![0], dir.join("Vox_01.wav"), 48_000).unwrap();
        assert!(TakeWriter::create(TrackId(3), vec![0], dir.join("Vox_01.wav"), 48_000).is_err(), "never overwrites a file");
        vox.write(&[0.5; 300], 3);
        vox.write_header();
        vox.write(&[0.25; 150], 3);
        assert_eq!(decode(&dir.join("Vox_01.wav"))[0].len(), 100, "the header covers what it has seen");

        let shared = Arc::new(Shared::new(CaptureRing::new(1 << 16)));
        shared.armed.store(true, Ordering::Release);
        let sh = Arc::clone(&shared);
        let writer = std::thread::spawn(move || write_takes(&sh, vec![gtr, vox], 3));
        let input: Vec<f32> = (0..3 * 4_800).map(|i| i as f32 / 20_000.0).collect();
        let mut owed = 0;
        input.chunks(3 * 480).for_each(|block| shared.capture(block, &mut owed));
        shared.armed.store(false, Ordering::Release);
        let (done, problem) = writer.join().unwrap();
        let channel = |c: usize| input.iter().skip(c).step_by(3).copied().collect::<Vec<f32>>();
        assert_eq!((done.len(), problem), (2, None));
        assert_eq!(decode(&done[0].1), vec![channel(1), channel(2)]);
        assert_eq!(decode(&done[1].1), vec![[vec![0.5; 100], vec![0.25; 50], channel(0)].concat()]);
    }

    #[test]
    fn a_take_sums_itself_up_for_drawing_while_it_records() {
        let dir = test_dir("live-take");
        let mut take = TakeWriter::create(TrackId(4), vec![2, 0], dir.join("Gtr_01.wav"), 48_000).unwrap();
        let input: Vec<f32> = (0..300).flat_map(|f| [f as f32, -1.0, -(f as f32)]).collect();
        take.write(&input[..3 * 100], 3);
        take.write(&input[3 * 100..], 3);
        let live = take.live.lock().unwrap().channels.clone();
        assert_eq!(live, [[(-127.0, 0.0), (-255.0, -128.0), (-299.0, -256.0)], [(0.0, 127.0), (128.0, 255.0), (256.0, 299.0)]]);
    }
}

/// Ring state behind the lock: interleaved input frames at the input device's rate, plus the
/// fractional read position (in input frames) of the next output sample.
/// Lock-free SPSC monitor FIFO. The input callback owns head, output owns tail/phase.
/// Full queues drop incoming whole frames; the producer never changes the consumer cursor.
pub struct InputRing {
    pub(crate) timing: Arc<crate::latency::LatencyTracker>,
    slots: Box<[AtomicU32]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    phase: AtomicU64,
    frame_mask: usize,
    pub channels: AtomicUsize,
    rate: u32,
    /// Samples physically reserved, always a whole number of channel frames.
    cap: usize,
}

impl InputRing {
    pub fn new(channels: usize, rate: u32) -> Arc<InputRing> {
        let ch = channels.clamp(1, 256);
        let frames = if rate == 0 { 24_000 } else { (rate as usize / 2).clamp(2, 384_000) };
        let frames = frames.min((1usize << 20) / ch).max(2);
        let frame_capacity = 1usize << (usize::BITS - 1 - frames.leading_zeros());
        let cap = frame_capacity.saturating_mul(ch);
        Arc::new(InputRing {
            timing: Arc::default(),
            slots: (0..cap).map(|_| AtomicU32::new(0)).collect(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            phase: AtomicU64::new(0.0f64.to_bits()),
            frame_mask: frame_capacity - 1,
            channels: AtomicUsize::new(ch),
            rate,
            cap,
        })
    }

    pub fn push(&self, samples: &[f32]) {
        let channels = self.channels.load(Ordering::Relaxed).max(1);
        if channels > self.cap {
            return;
        }
        let mut head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let room = (self.frame_mask + 1).saturating_sub(head.wrapping_sub(tail));
        let count = (samples.len() / channels).min(room);
        for (i, sample) in samples.iter().take(count.saturating_mul(channels)).enumerate() {
            let frame = head.wrapping_add(i / channels) & self.frame_mask;
            if let Some(slot) = self.slots.get(frame.saturating_mul(channels).saturating_add(i % channels)) {
                slot.store(finite(*sample).to_bits(), Ordering::Relaxed);
            }
        }
        head = head.wrapping_add(count);
        self.head.store(head, Ordering::Release);
    }

    /// Consumer-only: forget queued monitor frames and interpolation phase without touching
    /// producer state or storage. Frames published concurrently remain for the next callback.
    pub fn discard_pending(&self) {
        self.tail.store(self.head.load(Ordering::Acquire), Ordering::Release);
        self.phase.store(0.0f64.to_bits(), Ordering::Relaxed);
    }

    /// Consumer-only linear sample-rate conversion into existing planar output buffers.
    pub fn pop_into(&self, out: &mut [Vec<f32>], frames: usize, session_rate: u32) {
        for channel in out.iter_mut() {
            channel.iter_mut().take(frames).for_each(|sample| *sample = 0.0);
        }
        let ch = self.channels.load(Ordering::Relaxed).max(1);
        if ch > self.cap {
            return;
        }
        let mut tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let mut phase = f64::from_bits(self.phase.load(Ordering::Relaxed));
        let step = if self.rate == 0 || session_rate == 0 { 1.0 } else { f64::from(self.rate) / f64::from(session_rate) };
        for frame in 0..frames {
            while phase >= 1.0 && head.wrapping_sub(tail) >= 2 {
                tail = tail.wrapping_add(1);
                phase -= 1.0;
            }
            if phase >= 1.0 || head.wrapping_sub(tail) < 2 {
                break;
            }
            let fraction = phase as f32;
            for channel in 0..ch {
                let get = |frame: usize| {
                    self.slots
                        .get((frame & self.frame_mask).saturating_mul(ch).saturating_add(channel))
                        .map_or(0.0, |slot| f32::from_bits(slot.load(Ordering::Relaxed)))
                };
                let a = get(tail);
                let b = get(tail.wrapping_add(1));
                if let Some(sample) = out.get_mut(channel).and_then(|values| values.get_mut(frame)) {
                    *sample = a + (b - a) * fraction;
                }
            }
            phase += step;
        }
        self.phase.store(phase.to_bits(), Ordering::Relaxed);
        self.tail.store(tail, Ordering::Release);
    }
}

#[cfg(test)]
mod ring_tests {
    use super::InputRing;

    #[test]
    fn monitor_discard_resets_backlog_and_fractional_phase() {
        let ring = InputRing::new(1, 48_000);
        ring.push(&[0.9; 32]);
        let mut output = vec![vec![0.0; 4]];
        ring.pop_into(&mut output, 1, 44_100);
        ring.discard_pending();
        ring.pop_into(&mut output, 4, 44_100);
        assert!(output[0].iter().all(|x| *x == 0.0));
        ring.push(&[0.2; 32]);
        ring.pop_into(&mut output, 4, 48_000);
        assert!(output[0].iter().all(|x| (*x - 0.2).abs() < 1e-6));
    }

    #[test]
    fn oversized_monitor_callback_keeps_memory_bounded_and_channels_aligned() {
        let ring = InputRing::new(2, 48_000);
        let capacity = ring.slots.len();
        ring.push(&(0..200_000).map(|n| n as f32).collect::<Vec<_>>());
        assert_eq!(ring.slots.len(), capacity);
        assert_eq!(ring.head.load(std::sync::atomic::Ordering::Acquire), ring.cap / 2);
        assert_eq!(ring.tail.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(f32::from_bits(ring.slots[0].load(std::sync::atomic::Ordering::Relaxed)), 0.0);
        assert_eq!(ring.cap % 2, 0);
    }

    #[test]
    fn full_monitor_drops_new_whole_frames_without_changing_consumer_cursor() {
        use std::sync::atomic::Ordering;
        let ring = InputRing::new(3, 8);
        ring.push(&(0..18).map(|n| n as f32).collect::<Vec<_>>());
        assert_eq!(ring.head.load(Ordering::Acquire), 4);
        assert_eq!(ring.tail.load(Ordering::Relaxed), 0);
        ring.push(&[90.0, 91.0, 92.0]);
        assert_eq!(ring.tail.load(Ordering::Relaxed), 0);
        let mut out = vec![vec![0.0; 3]; 3];
        ring.pop_into(&mut out, 3, 8);
        assert_eq!(out, [vec![0.0, 3.0, 6.0], vec![1.0, 4.0, 7.0], vec![2.0, 5.0, 8.0]]);
    }

    #[test]
    fn monitor_frame_cursors_wrap_without_mixing_channels() {
        use std::sync::atomic::Ordering;
        let ring = InputRing::new(3, 16);
        ring.head.store(usize::MAX - 1, Ordering::Relaxed);
        ring.tail.store(usize::MAX - 1, Ordering::Relaxed);
        ring.push(&[1.0, 11.0, 21.0, 2.0, 12.0, 22.0, 3.0, 13.0, 23.0, 4.0, 14.0, 24.0]);
        let mut out = vec![vec![0.0; 3]; 3];
        ring.pop_into(&mut out, 3, 16);
        assert_eq!(out, [vec![1.0, 2.0, 3.0], vec![11.0, 12.0, 13.0], vec![21.0, 22.0, 23.0]]);
    }

    /// A 1 kHz sine recorded at 16 kHz must come out as a 1 kHz sine at 48 kHz, across calls.
    #[test]
    fn monitor_converts_input_rate_to_session_rate() {
        let ring = InputRing::new(1, 16_000);
        let input: Vec<f32> = (0..4_000).map(|i| (2.0 * std::f64::consts::PI * 1_000.0 * f64::from(i) / 16_000.0).sin() as f32).collect();
        ring.push(&input);
        let mut out = vec![vec![0.0f32; 48]];
        let mut k = 0usize;
        for _ in 0..10 {
            ring.pop_into(&mut out, 48, 48_000);
            for v in &out[0] {
                let want = (2.0 * std::f64::consts::PI * 1_000.0 * k as f64 / 48_000.0).sin() as f32;
                assert!((v - want).abs() < 0.05, "sample {k}: got {v}, want {want}");
                k += 1;
            }
        }
    }

    /// Input faster than the session (96 kHz into 48 kHz): every other frame is read, in order.
    #[test]
    fn monitor_decimates_when_input_is_faster() {
        let ring = InputRing::new(1, 96_000);
        let input: Vec<f32> = (0..1_000).map(|i| i as f32).collect();
        ring.push(&input);
        let mut out = vec![vec![0.0f32; 100]];
        ring.pop_into(&mut out, 100, 48_000);
        for (k, v) in out[0].iter().enumerate() {
            assert_eq!(*v, (2 * k) as f32, "output {k}");
        }
    }

    /// Same rate: samples pass through unchanged.
    #[test]
    fn monitor_passes_through_at_equal_rates() {
        let ring = InputRing::new(2, 48_000);
        let input: Vec<f32> = (0..200).map(|i| i as f32).collect();
        ring.push(&input);
        let mut out = vec![vec![0.0f32; 50], vec![0.0f32; 50]];
        ring.pop_into(&mut out, 50, 48_000);
        assert_eq!(out[0][0], 0.0);
        assert_eq!(out[1][0], 1.0);
        assert_eq!(out[0][49], 98.0);
    }

    /// Nothing buffered: silence, not stale data.
    #[test]
    fn monitor_underrun_is_silence() {
        let ring = InputRing::new(1, 16_000);
        let mut out = vec![vec![1.0f32; 16]];
        ring.pop_into(&mut out, 16, 48_000);
        assert!(out[0].iter().all(|v| *v == 0.0));
    }

    /// NaN and infinities from the device monitor as silence.
    #[test]
    fn monitor_silences_nan_and_infinite_input() {
        let ring = InputRing::new(1, 48_000);
        ring.push(&[0.5, f32::NAN, f32::INFINITY, -0.5, 0.0, 0.0, 0.0, 0.0]);
        let mut out = vec![vec![1.0f32; 4]];
        ring.pop_into(&mut out, 4, 48_000);
        assert_eq!(out[0], [0.5, 0.0, 0.0, -0.5]);
    }
}
