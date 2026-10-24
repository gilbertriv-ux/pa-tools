//! Safe ownership of paired input/output streams.
//!
//! The portable backend uses independent device clocks. Matching nominal rates does not
//! establish sample alignment; monitoring resamples through `InputRing`. Native ASIO shared
//! driver setup and hardware loopback verification remain separate work.
use crate::{AudioConfig, Player, latency::StreamLatency, record::Recorder};
use soundcraft_model::Session;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockRelationship {
    /// No shared hardware clock or sample alignment has been verified.
    Independent,
    /// Input timestamps are resampled onto the output presentation clock in software.
    /// This does not assert a shared physical oscillator.
    SoftwareMaster,
}

pub trait DuplexStream {
    fn player(&self) -> &Player;
    fn recorder(&self) -> &Recorder;
    fn recorder_mut(&mut self) -> &mut Recorder;
    fn clock_relationship(&self) -> ClockRelationship;
    fn input_latency(&self) -> Option<StreamLatency>;
    fn output_latency(&self) -> Option<StreamLatency>;
}

/// Owns both streams so failed input setup cannot leave a successful duplex result.
/// Recording continues to use a bounded ring and a disk writer.
pub struct PortableDuplex {
    recorder: Recorder,
    player: Player,
}

impl PortableDuplex {
    pub fn open(session: Arc<Session>, config: &AudioConfig) -> Result<Self, String> {
        if config.system.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("ASIO")) {
            return Err("native shared-clock duplex is not implemented for this audio system".into());
        }
        let player = Player::with_config(session, config);
        if player.silent {
            return Err("cannot open duplex without an audio output device".into());
        }
        let mut input = config.clone();
        input.sample_rate = Some(player.device_rate);
        input.buffer_size = player.device_buffer;
        let recorder = Recorder::open_with(&input)?;
        player.set_input(Arc::clone(&recorder.monitor));
        Ok(Self { recorder, player })
    }
}

impl DuplexStream for PortableDuplex {
    fn player(&self) -> &Player {
        &self.player
    }
    fn recorder(&self) -> &Recorder {
        &self.recorder
    }
    fn recorder_mut(&mut self) -> &mut Recorder {
        &mut self.recorder
    }
    fn clock_relationship(&self) -> ClockRelationship {
        ClockRelationship::Independent
    }
    fn input_latency(&self) -> Option<StreamLatency> {
        self.recorder.input_latency()
    }
    fn output_latency(&self) -> Option<StreamLatency> {
        self.player.output_latency()
    }
}

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Output-callback-owned clock. All fields are atomic, and readers try at most four snapshots.
/// There is exactly one writer: the output callback. Neither operation allocates or locks.
#[derive(Debug)]
pub struct MasterClock {
    sequence: AtomicU64,
    valid: AtomicBool,
    recording: AtomicBool,
    generation: AtomicU64,
    rate: AtomicU32,
    cursor: AtomicU64,
    latest_ns: AtomicU64,
    latest_frame: AtomicU64,
    anchor_ns: AtomicU64,
    anchor_frame: AtomicU64,
    calibration_ns: AtomicU64,
    calibration_frame: AtomicU64,
    slope: AtomicU64,
}

impl Default for MasterClock {
    fn default() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            valid: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            rate: AtomicU32::new(0),
            cursor: AtomicU64::new(0),
            latest_ns: AtomicU64::new(0),
            latest_frame: AtomicU64::new(0),
            anchor_ns: AtomicU64::new(0),
            anchor_frame: AtomicU64::new(0),
            calibration_ns: AtomicU64::new(0),
            calibration_frame: AtomicU64::new(0),
            slope: AtomicU64::new(0),
        }
    }
}

impl MasterClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Publish the presentation timestamp of this output block's FIRST frame. `frames` advances
    /// the next block cursor, not this block's timestamp. Call from the sole output writer.
    pub fn publish(&self, playback_ns: u64, frames: usize, rate: u32, recording: bool) {
        if playback_ns == 0 || frames == 0 || rate == 0 {
            self.invalidate();
            return;
        }
        self.sequence.fetch_add(1, Ordering::SeqCst);
        let was_valid = self.valid.load(Ordering::SeqCst);
        let old_rate = self.rate.load(Ordering::SeqCst);
        let cursor = self.cursor.load(Ordering::SeqCst);
        let restart = !was_valid || old_rate != rate || playback_ns <= self.latest_ns.load(Ordering::SeqCst);
        let nominal = f64::from(rate) / 1_000_000_000.0;
        let mut slope = nominal;
        if restart {
            self.calibration_ns.store(playback_ns, Ordering::SeqCst);
            self.calibration_frame.store(cursor, Ordering::SeqCst);
        } else {
            let base_ns = self.calibration_ns.load(Ordering::SeqCst);
            let base_frame = self.calibration_frame.load(Ordering::SeqCst);
            let elapsed_ns = playback_ns.saturating_sub(base_ns);
            let elapsed_frames = cursor.saturating_sub(base_frame);
            // A longer baseline filters timestamp rounding/jitter without assuming nominal rate.
            if elapsed_ns >= 100_000_000 && elapsed_frames > 0 {
                let measured = elapsed_frames as f64 / elapsed_ns as f64;
                if measured.is_finite() && (0.98..=1.02).contains(&(measured / nominal)) {
                    slope = measured;
                }
            }
        }
        if recording && (!self.recording.load(Ordering::SeqCst) || restart) {
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.anchor_ns.store(playback_ns, Ordering::SeqCst);
            self.anchor_frame.store(cursor, Ordering::SeqCst);
        }
        self.latest_ns.store(playback_ns, Ordering::SeqCst);
        self.latest_frame.store(cursor, Ordering::SeqCst);
        self.cursor.store(cursor.saturating_add(frames as u64), Ordering::SeqCst);
        self.slope.store(slope.to_bits(), Ordering::SeqCst);
        self.rate.store(rate, Ordering::SeqCst);
        self.recording.store(recording, Ordering::SeqCst);
        self.valid.store(true, Ordering::SeqCst);
        self.sequence.fetch_add(1, Ordering::SeqCst);
    }

    /// Invalidate on missing timestamps, device changes or output failure. Sole writer only.
    pub fn invalidate(&self) {
        self.sequence.fetch_add(1, Ordering::SeqCst);
        self.valid.store(false, Ordering::SeqCst);
        self.recording.store(false, Ordering::SeqCst);
        self.sequence.fetch_add(1, Ordering::SeqCst);
    }

    /// Control-side arm gate: never accept a prior active take while a snapshot is busy.
    pub fn next_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst).saturating_add(1)
    }

    pub fn snapshot(&self) -> Option<ClockSnapshot> {
        for _ in 0..4 {
            let before = self.sequence.load(Ordering::SeqCst);
            if before & 1 != 0 {
                continue;
            }
            let valid = self.valid.load(Ordering::SeqCst);
            let snapshot = ClockSnapshot {
                generation: self.generation.load(Ordering::SeqCst),
                recording: self.recording.load(Ordering::SeqCst),
                anchor_ns: self.anchor_ns.load(Ordering::SeqCst),
                rate: self.rate.load(Ordering::SeqCst),
                latest_ns: self.latest_ns.load(Ordering::SeqCst),
                latest_frame: self.latest_frame.load(Ordering::SeqCst),
                anchor_frame: self.anchor_frame.load(Ordering::SeqCst),
                frames_per_ns: f64::from_bits(self.slope.load(Ordering::SeqCst)),
            };
            if before == self.sequence.load(Ordering::SeqCst) {
                return valid.then_some(snapshot);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClockSnapshot {
    pub generation: u64,
    pub recording: bool,
    pub anchor_ns: u64,
    pub rate: u32,
    latest_ns: u64,
    latest_frame: u64,
    anchor_frame: u64,
    frames_per_ns: f64,
}

impl ClockSnapshot {
    /// Signed output-frame coordinate of a capture timestamp relative to the current take.
    pub fn frame_at(&self, capture_ns: u64) -> Option<f64> {
        if self.rate == 0 || !self.frames_per_ns.is_finite() || self.frames_per_ns <= 0.0 {
            return None;
        }
        let delta = if capture_ns >= self.latest_ns { (capture_ns - self.latest_ns) as f64 } else { -((self.latest_ns - capture_ns) as f64) };
        let frame = self.latest_frame as f64 - self.anchor_frame as f64 + delta * self.frames_per_ns;
        (frame.is_finite() && frame.abs() < 4_503_599_627_370_496.0).then_some(frame)
    }
}

const MAX_CHANNELS: usize = 64;
const MAX_INPUT_FRAMES: usize = 8192;
const MAX_EMIT_FRAMES: usize = 32768;
const MAX_GAP_FRAMES: usize = 4096;
// One nanosecond of timestamp rounding is 0.000048 frames at 48 kHz.
const FRAME_EPSILON: f64 = 0.000_1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub emitted_frames: usize,
    pub trimmed_frames: usize,
    pub padded_frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncError {
    InvalidBlock,
    InvalidClock,
    GapTooLarge,
}

/// Fixed-storage, timestamp-driven linear resampler for capture/monitoring. Construct it on the
/// control thread. `process` performs no allocation, locking, waiting, or unbounded gap filling.
/// It corrects relative device drift; this is software synchronization, not a hardware clock.
pub struct TimestampResampler {
    channels: usize,
    input_rate: u32,
    output_rate: u32,
    generation: u64,
    active: bool,
    next_frame: u64,
    previous: [f32; MAX_CHANNELS],
    current: [f32; MAX_CHANNELS],
    interpolated: [f32; MAX_CHANNELS],
    previous_position: f64,
    have_previous: bool,
    previous_block_ns: u64,
    previous_block_frames: usize,
    last_step: f64,
}

impl TimestampResampler {
    pub fn new(channels: usize, input_rate: u32, output_rate: u32) -> Result<Self, &'static str> {
        if channels == 0 || channels > MAX_CHANNELS || input_rate == 0 || output_rate == 0 {
            return Err("capture requires 1..64 channels and nonzero sample rates");
        }
        let step = f64::from(output_rate) / f64::from(input_rate);
        if !(1.0 / 16.0..=16.0).contains(&step) {
            return Err("capture conversion ratio exceeds the bounded callback limit");
        }
        Ok(Self {
            channels,
            input_rate,
            output_rate,
            generation: 0,
            active: false,
            next_frame: 0,
            previous: [0.0; MAX_CHANNELS],
            current: [0.0; MAX_CHANNELS],
            interpolated: [0.0; MAX_CHANNELS],
            previous_position: 0.0,
            have_previous: false,
            previous_block_ns: 0,
            previous_block_frames: 0,
            last_step: step,
        })
    }

    pub fn process(
        &mut self,
        input: &[f32],
        capture_start_ns: u64,
        clock: ClockSnapshot,
        mut emit: impl FnMut(&[f32]),
    ) -> Result<SyncReport, SyncError> {
        let mut report = SyncReport::default();
        if !clock.recording {
            self.active = false;
            self.have_previous = false;
            self.previous_block_frames = 0;
            return Ok(report);
        }
        if clock.rate != self.output_rate || capture_start_ns == 0 {
            return Err(SyncError::InvalidClock);
        }
        if input.len() % self.channels != 0 || input.len() / self.channels > MAX_INPUT_FRAMES {
            return Err(SyncError::InvalidBlock);
        }
        let frames = input.len() / self.channels;
        if frames == 0 {
            return Ok(report);
        }
        let start = clock.frame_at(capture_start_ns).ok_or(SyncError::InvalidClock)?;
        let nominal = f64::from(self.output_rate) / f64::from(self.input_rate);
        if !self.active || self.generation != clock.generation {
            self.active = true;
            self.generation = clock.generation;
            self.next_frame = 0;
            self.have_previous = false;
            self.previous_block_frames = 0;
            self.last_step = nominal;
        }
        let mut step = self.last_step;
        if self.previous_block_frames > 0 && capture_start_ns > self.previous_block_ns {
            let previous_start = clock.frame_at(self.previous_block_ns).ok_or(SyncError::InvalidClock)?;
            let observed = (start - previous_start) / self.previous_block_frames as f64;
            if observed.is_finite() && (0.98..=1.02).contains(&(observed / nominal)) {
                step = observed;
            }
        }
        let end = start + (frames - 1) as f64 * step;
        let possible = (end + FRAME_EPSILON).floor() - self.next_frame as f64 + 1.0;
        if !end.is_finite() || possible > MAX_EMIT_FRAMES as f64 {
            return Err(SyncError::GapTooLarge);
        }
        let gap = (!self.have_previous && start > self.next_frame as f64 + FRAME_EPSILON)
            || (self.have_previous && start - self.previous_position > self.last_step * 1.5 + FRAME_EPSILON);
        if gap {
            let until = (start - FRAME_EPSILON).ceil().max(0.0) as u64;
            let missing = until.saturating_sub(self.next_frame);
            if missing > MAX_GAP_FRAMES as u64 {
                return Err(SyncError::GapTooLarge);
            }
            let zero = [0.0; MAX_CHANNELS];
            for _ in 0..missing {
                emit(&zero[..self.channels]);
                self.next_frame = self.next_frame.saturating_add(1);
                report.emitted_frames += 1;
                report.padded_frames += 1;
            }
            self.have_previous = false;
        }
        let mut advanced = false;
        for (index, frame) in input.chunks_exact(self.channels).enumerate() {
            let position = start + index as f64 * step;
            if self.have_previous && position <= self.previous_position + FRAME_EPSILON {
                report.trimmed_frames += 1;
                continue;
            }
            for (destination, value) in self.current[..self.channels].iter_mut().zip(frame) {
                *destination = if value.is_finite() { *value } else { 0.0 };
            }
            if position < -FRAME_EPSILON {
                report.trimmed_frames += 1;
            }
            while self.next_frame as f64 <= position + FRAME_EPSILON {
                if report.emitted_frames >= MAX_EMIT_FRAMES {
                    return Err(SyncError::GapTooLarge);
                }
                let target = self.next_frame as f64;
                if self.have_previous {
                    let distance = position - self.previous_position;
                    let alpha = ((target - self.previous_position) / distance).clamp(0.0, 1.0) as f32;
                    for ((out, a), b) in
                        self.interpolated[..self.channels].iter_mut().zip(&self.previous[..self.channels]).zip(&self.current[..self.channels])
                    {
                        let value = *a + (*b - *a) * alpha;
                        *out = if value.is_finite() { value } else { 0.0 };
                    }
                    emit(&self.interpolated[..self.channels]);
                } else {
                    // The first sample is only used at its own coordinate; any earlier gap
                    // was explicitly zero-filled above rather than silently time-shifted.
                    emit(&self.current[..self.channels]);
                }
                self.next_frame = self.next_frame.saturating_add(1);
                report.emitted_frames += 1;
            }
            self.previous[..self.channels].copy_from_slice(&self.current[..self.channels]);
            self.previous_position = position;
            self.have_previous = true;
            advanced = true;
        }
        if advanced && capture_start_ns >= self.previous_block_ns {
            self.previous_block_ns = capture_start_ns;
            self.previous_block_frames = frames;
            self.last_step = step;
        }
        Ok(report)
    }
}

/// Physical destination -> source-channel mapping. Duplicate source channels intentionally
/// support fanout; None mutes a destination. Validation occurs outside the callback.
#[derive(Debug, Clone)]
pub struct ChannelMap {
    sources: [Option<usize>; MAX_CHANNELS],
    destinations: usize,
}
impl ChannelMap {
    pub fn new(map: Vec<Option<usize>>, source_channels: usize, device_channels: usize) -> Result<Self, &'static str> {
        if source_channels == 0
            || source_channels > MAX_CHANNELS
            || device_channels == 0
            || device_channels > MAX_CHANNELS
            || map.len() != device_channels
        {
            return Err("routing requires 1..64 channels and exactly one entry per destination");
        }
        if map.iter().flatten().any(|source| *source >= source_channels) {
            return Err("routing source channel is outside the source layout");
        }
        let mut sources = [None; MAX_CHANNELS];
        sources[..device_channels].copy_from_slice(&map);
        Ok(Self { sources, destinations: device_channels })
    }
    pub fn source_for(&self, destination: usize) -> Option<usize> {
        self.sources.get(destination).copied().flatten().filter(|_| destination < self.destinations)
    }
    pub fn apply(&self, input_frame: &[f32], output_frame: &mut [f32]) {
        for (destination, out) in output_frame.iter_mut().enumerate() {
            let value = self.source_for(destination).and_then(|source| input_frame.get(source)).copied().unwrap_or(0.0);
            *out = if value.is_finite() { value } else { 0.0 };
        }
    }
}

#[cfg(test)]
mod software_clock_tests {
    use super::*;
    const START: u64 = 1_000_000_000;
    fn stamp(frame: f64, rate: u32) -> u64 {
        START + (frame * 1e9 / f64::from(rate)).round() as u64
    }
    fn clock(rate: u32) -> (Arc<MasterClock>, ClockSnapshot) {
        let clock = MasterClock::new();
        clock.publish(START, 128, rate, true);
        let snapshot = clock.snapshot().unwrap();
        (clock, snapshot)
    }
    #[test]
    fn master_anchor_rises_once_and_invalidates_without_spinning() {
        let c = MasterClock::new();
        assert!(c.snapshot().is_none());
        c.publish(START, 480, 48000, false);
        assert!(!c.snapshot().unwrap().recording);
        c.publish(START + 10_000_000, 480, 48000, true);
        let first = c.snapshot().unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(first.frame_at(START + 10_000_000), Some(0.0));
        c.publish(START + 20_000_000, 480, 48000, true);
        assert_eq!(c.snapshot().unwrap().generation, 1);
        assert!((c.snapshot().unwrap().frame_at(START + 10_000_000).unwrap()).abs() < 1e-8);
        c.invalidate();
        assert!(c.snapshot().is_none());
        c.publish(START + 30_000_000, 480, 48000, true);
        assert_eq!(c.snapshot().unwrap().generation, 2);
        c.sequence.store(1, Ordering::SeqCst);
        assert!(c.snapshot().is_none(), "an in-progress writer never forces a spinning reader");
    }
    #[test]
    fn preroll_is_trimmed_and_record_generation_resets_alignment() {
        let (master, snapshot) = clock(48000);
        let mut r = TimestampResampler::new(1, 48000, 48000).unwrap();
        let mut output = Vec::new();
        let report = r.process(&[-2.0, -1.0, 0.0, 1.0, 2.0], START - 41667, snapshot, |f| output.extend_from_slice(f)).unwrap();
        assert_eq!(report.trimmed_frames, 2);
        assert_eq!(output.len(), 3);
        assert!(output[0].abs() < 0.0001);
        master.publish(START + 10_000_000, 128, 48000, false);
        r.process(&[9.0], START + 10_000_000, master.snapshot().unwrap(), |_| panic!("not recording")).unwrap();
        master.publish(START + 20_000_000, 128, 48000, true);
        let mut new_take = Vec::new();
        r.process(&[7.0, 8.0], START + 20_000_000, master.snapshot().unwrap(), |f| new_take.extend_from_slice(f)).unwrap();
        assert_eq!(new_take, [7.0, 8.0]);
    }
    #[test]
    fn gaps_are_bounded_and_overlap_is_trimmed() {
        let (_, snapshot) = clock(48000);
        let mut r = TimestampResampler::new(1, 48000, 48000).unwrap();
        let mut out = Vec::new();
        r.process(&[0.0, 1.0, 2.0, 3.0], START, snapshot, |f| out.extend_from_slice(f)).unwrap();
        let overlap = r.process(&[2.0, 3.0, 4.0, 5.0], stamp(2.0, 48000), snapshot, |f| out.extend_from_slice(f)).unwrap();
        assert_eq!(overlap.trimmed_frames, 2);
        let gap = r.process(&[8.0, 9.0], stamp(8.0, 48000), snapshot, |f| out.extend_from_slice(f)).unwrap();
        assert_eq!(gap.padded_frames, 2);
        for (actual, expected) in out.iter().zip([0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 0.0, 0.0, 8.0, 9.0]) {
            assert!((actual - expected).abs() < 0.0001);
        }
        assert_eq!(out.len(), 10);
        assert_eq!(r.process(&[1.0], START + 1_000_000_000, snapshot, |_| panic!("large gap emitted")), Err(SyncError::GapTooLarge));
    }
    #[test]
    fn chunk_boundaries_do_not_change_fixed_rate_interpolation() {
        let (_, snapshot) = clock(48000);
        let source: Vec<_> = (0..20).map(|i| i as f32).collect();
        let mut whole = TimestampResampler::new(1, 24000, 48000).unwrap();
        let mut chunks = TimestampResampler::new(1, 24000, 48000).unwrap();
        let mut a = Vec::new();
        let mut b = Vec::new();
        whole.process(&source, START, snapshot, |f| a.extend_from_slice(f)).unwrap();
        for (n, part) in source.chunks(5).enumerate() {
            chunks.process(part, stamp((n * 5 * 2) as f64, 48000), snapshot, |f| b.extend_from_slice(f)).unwrap();
        }
        assert_eq!(a.len(), 39);
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(&b) {
            assert!((a - b).abs() < 0.0001);
        }
    }
    #[test]
    fn timestamp_rate_estimation_corrects_plus_and_minus_100_ppm() {
        for ppm in [-100.0, 100.0] {
            let actual_input = 48000.0 * (1.0 + ppm / 1e6);
            let step = 48000.0 / actual_input;
            let master = MasterClock::new();
            let mut r = TimestampResampler::new(1, 48000, 48000).unwrap();
            let mut count = 0usize;
            let mut worst = 0.0f64;
            for block in 0..500 {
                master.publish(stamp((block * 128) as f64, 48000), 128, 48000, true);
                let mut input = [0.0; 128];
                for (i, sample) in input.iter_mut().enumerate() {
                    *sample = ((block * 128 + i) as f64 * step) as f32;
                }
                let capture = START + ((block * 128) as f64 * 1e9 / actual_input).round() as u64;
                r.process(&input, capture, master.snapshot().unwrap(), |f| {
                    worst = worst.max((f64::from(f[0]) - count as f64).abs());
                    count += 1;
                })
                .unwrap();
            }
            assert!(worst < 0.04, "ppm={ppm} maximum alignment error {worst} frames");
            assert!((count as f64 - (63999.0 * step).floor() - 1.0).abs() <= 1.0);
        }
    }
    #[test]
    fn invalid_blocks_and_routes_are_rejected_without_emission() {
        assert!(TimestampResampler::new(0, 48000, 48000).is_err());
        let (_, snapshot) = clock(48000);
        let mut r = TimestampResampler::new(2, 48000, 48000).unwrap();
        assert_eq!(r.process(&[1.0], START, snapshot, |_| panic!("invalid input emitted")), Err(SyncError::InvalidBlock));
        assert!(ChannelMap::new(vec![Some(2)], 2, 1).is_err());
        assert!(ChannelMap::new(vec![Some(0)], 2, 2).is_err());
        let map = ChannelMap::new(vec![Some(2), Some(0), None, Some(2)], 3, 4).unwrap();
        let mut out = [9.0; 4];
        map.apply(&[1.0, 2.0, 3.0], &mut out);
        assert_eq!(out, [3.0, 1.0, 0.0, 3.0]);
        assert_eq!(map.source_for(4), None);
    }
}
