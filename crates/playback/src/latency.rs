//! Audio latency reporting and record-offset compensation (G-01).
//!
//! cpal 0.16 has no duplex stream, so input and output latency are measured separately, each from
//! its own callback timestamps (the gap between "the callback runs" and "the first frame reaches
//! the DAC" / "the first frame left the ADC"). Everything here is a pure function over durations
//! and integers so it can be tested without hardware. The device's own driver-reported figures
//! (safety offsets, converter delay) are not visible through cpal; the estimate covers the
//! buffering the host reports in its timestamps plus the configured device buffer.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// Upper bound for any single latency figure, in frames (about 21 s at 48 kHz). Hostile or
/// garbage timestamps are clamped to this.
pub const MAX_LATENCY_FRAMES: u32 = 1 << 20;

/// Latency of one device pair, in frames at `sample_rate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LatencyReport {
    /// Frames between a sound reaching the input and the callback seeing it.
    pub input_frames: u32,
    /// Frames between the output callback writing a frame and it sounding.
    pub output_frames: u32,
    /// The configured device buffer in frames (0 when the device default is used or unknown).
    pub device_buffer_frames: u32,
    pub sample_rate: u32,
}

impl LatencyReport {
    /// Input plus output latency in frames, plus one device buffer of software slack when the
    /// buffer is known and the measured figures do not already cover it. Saturating.
    pub fn round_trip_frames(&self) -> u32 {
        let measured = self.input_frames.saturating_add(self.output_frames);
        let floor = self.device_buffer_frames;
        // Timestamps that report nothing (some hosts) fall back to one buffer each way.
        if measured == 0 { floor.saturating_mul(2).min(MAX_LATENCY_FRAMES) } else { measured.min(MAX_LATENCY_FRAMES) }
    }

    /// Round trip in milliseconds; 0.0 when the sample rate is unknown.
    pub fn round_trip_ms(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        f64::from(self.round_trip_frames()) * 1000.0 / f64::from(self.sample_rate)
    }

    /// How many frames earlier a take should be placed so it lines up with what was heard.
    pub fn record_offset_frames(&self) -> i64 {
        i64::from(self.round_trip_frames())
    }
}

/// Frames in `d` at `rate`, rounded to nearest, clamped to [`MAX_LATENCY_FRAMES`]. 0 for rate 0.
pub fn duration_to_frames(d: Duration, rate: u32) -> u32 {
    let frames = d.as_secs_f64() * f64::from(rate);
    if !frames.is_finite() || frames <= 0.0 {
        return 0;
    }
    (frames.round().min(f64::from(MAX_LATENCY_FRAMES))) as u32
}

/// Output latency from a callback's two instants, expressed as the time from "callback" to
/// "playback" (`None` when the clock went backwards or the host gave nothing).
pub fn output_latency_frames(callback_to_playback: Option<Duration>, rate: u32) -> u32 {
    callback_to_playback.map_or(0, |d| duration_to_frames(d, rate))
}

/// Input latency from "capture" to "callback" (`None` as above).
pub fn input_latency_frames(capture_to_callback: Option<Duration>, rate: u32) -> u32 {
    capture_to_callback.map_or(0, |d| duration_to_frames(d, rate))
}

/// Shift a take's start earlier by `offset` frames, saturating; never moves before 0.
pub fn shifted_start(start: i64, offset: i64) -> i64 {
    start.saturating_sub(offset.max(0)).max(0)
}

/// Frames to trim from the head of a take when `start` cannot move earlier than 0 (the part of
/// the offset that did not fit), so the audio still lines up.
pub fn head_trim(start: i64, offset: i64) -> i64 {
    offset.max(0).saturating_sub(start.max(0)).max(0)
}

/// A latency figure written by an audio callback and read anywhere. Lock-free.
#[derive(Debug, Default)]
pub struct LatencyCell(AtomicU32);

impl LatencyCell {
    /// Store the most recent measurement (clamped).
    pub fn store(&self, frames: u32) {
        self.0.store(frames.min(MAX_LATENCY_FRAMES), Ordering::Relaxed);
    }

    pub fn load(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(i: u32, o: u32, b: u32) -> LatencyReport {
        LatencyReport { input_frames: i, output_frames: o, device_buffer_frames: b, sample_rate: 48_000 }
    }

    #[test]
    fn round_trip_adds_input_and_output() {
        let r = report(128, 256, 512);
        assert_eq!(r.round_trip_frames(), 384);
        assert_eq!(r.record_offset_frames(), 384);
        assert!((r.round_trip_ms() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn unmeasured_latency_falls_back_to_two_buffers() {
        assert_eq!(report(0, 0, 256).round_trip_frames(), 512);
        assert_eq!(report(0, 0, 0).round_trip_frames(), 0);
    }

    #[test]
    fn hostile_values_saturate() {
        let r = report(u32::MAX, u32::MAX, u32::MAX);
        assert_eq!(r.round_trip_frames(), MAX_LATENCY_FRAMES);
        assert_eq!(report(0, 0, u32::MAX).round_trip_frames(), MAX_LATENCY_FRAMES);
        let z = LatencyReport { sample_rate: 0, ..report(10, 10, 0) };
        assert_eq!(z.round_trip_ms(), 0.0);
    }

    #[test]
    fn durations_convert_to_frames() {
        assert_eq!(duration_to_frames(Duration::from_millis(10), 48_000), 480);
        assert_eq!(duration_to_frames(Duration::from_micros(10), 48_000), 0);
        assert_eq!(duration_to_frames(Duration::from_secs(10_000), 48_000), MAX_LATENCY_FRAMES);
        assert_eq!(duration_to_frames(Duration::from_secs(1), 0), 0);
        assert_eq!(output_latency_frames(None, 48_000), 0);
        assert_eq!(output_latency_frames(Some(Duration::from_millis(5)), 48_000), 240);
        assert_eq!(input_latency_frames(Some(Duration::from_millis(2)), 44_100), 88);
    }

    #[test]
    fn take_start_shifts_earlier_and_never_below_zero() {
        assert_eq!(shifted_start(1000, 384), 616);
        assert_eq!(shifted_start(100, 384), 0);
        assert_eq!(shifted_start(i64::MIN, 384), 0);
        assert_eq!(shifted_start(500, -50), 500, "a negative offset never delays");
        assert_eq!(shifted_start(i64::MAX, i64::MAX), 0);
        assert_eq!(head_trim(100, 384), 284);
        assert_eq!(head_trim(1000, 384), 0);
        assert_eq!(head_trim(0, -5), 0);
    }

    #[test]
    fn cell_keeps_the_latest_clamped_value() {
        let c = LatencyCell::default();
        assert_eq!(c.load(), 0);
        c.store(300);
        c.store(u32::MAX);
        assert_eq!(c.load(), MAX_LATENCY_FRAMES);
    }
}
