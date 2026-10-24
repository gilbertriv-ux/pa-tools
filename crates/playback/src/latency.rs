//! Callback timestamp latency, distinguished from a buffer-duration estimate.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencySource {
    /// Capture-to-callback or callback-to-playback timestamps supplied by the backend.
    CallbackTimestamp,
    /// One requested buffer; excludes unknown device/converter latency.
    BufferEstimate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLatency {
    pub duration: Duration,
    pub source: LatencySource,
}

#[derive(Default)]
pub(crate) struct LatencyTracker {
    // Zero means no usable timestamp yet; store nanoseconds + 1 to retain zero latency.
    nanos: AtomicU64,
}

impl LatencyTracker {
    pub(crate) fn observe(&self, duration: Option<Duration>) {
        if let Some(d) = duration {
            let n = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX - 1);
            self.nanos.store(n.saturating_add(1), Ordering::Relaxed);
        }
    }

    pub(crate) fn snapshot(&self, frames: Option<u32>, rate: u32) -> Option<StreamLatency> {
        match self.nanos.load(Ordering::Relaxed) {
            0 => buffer_latency(frames?, rate),
            n => Some(StreamLatency { duration: Duration::from_nanos(n - 1), source: LatencySource::CallbackTimestamp }),
        }
    }
}

/// The duration of a buffer, not a measurement of hardware latency.
pub fn buffer_latency(frames: u32, rate: u32) -> Option<StreamLatency> {
    if frames == 0 || rate == 0 {
        return None;
    }
    Some(StreamLatency { duration: Duration::from_secs_f64(f64::from(frames) / f64::from(rate)), source: LatencySource::BufferEstimate })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinguishes_unknown_estimated_and_timestamp_latency() {
        let t = LatencyTracker::default();
        assert_eq!(t.snapshot(None, 48_000), None);
        assert_eq!(buffer_latency(128, 0), None);
        assert_eq!(buffer_latency(0, 48_000), None);
        assert_eq!(t.snapshot(Some(480), 48_000).unwrap().duration, Duration::from_millis(10));
        t.observe(Some(Duration::ZERO));
        assert_eq!(t.snapshot(None, 48_000).unwrap().source, LatencySource::CallbackTimestamp);
        assert_eq!(t.snapshot(None, 48_000).unwrap().duration, Duration::ZERO);
        t.observe(None);
        assert_eq!(t.snapshot(None, 48_000).unwrap().duration, Duration::ZERO);
    }
}
