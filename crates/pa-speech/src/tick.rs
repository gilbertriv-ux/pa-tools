//! Internal exact media time, inlined from the donor's `filmcraft-time` (only the subset the
//! Whisper port uses). Public word bounds are `f64` seconds; ticks are used only inside the
//! feature-gated recogniser and converted at the trait boundary.

use crate::SAMPLE_RATE;

/// Ticks per second (divides evenly into every common sample and frame rate — exact alignment).
pub const TICKS_PER_SECOND: i64 = 254_016_000_000;
/// Ticks per 16 kHz sample (exact).
pub const TICKS_PER_SAMPLE: i64 = TICKS_PER_SECOND / SAMPLE_RATE as i64;

/// A point or duration in time, in ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tick(pub i64);

impl Tick {
    pub fn seconds(self) -> f64 {
        self.0 as f64 / TICKS_PER_SECOND as f64
    }
    pub fn max(self, o: Tick) -> Tick {
        Tick(self.0.max(o.0))
    }
}

// Arithmetic saturates at the i64 storage limits (as in the donor's `filmcraft-time`), so a
// damaged time never panics on overflow — the crate forbids panics anyway.
impl std::ops::Sub for Tick {
    type Output = Tick;
    fn sub(self, o: Tick) -> Tick {
        Tick(self.0.saturating_sub(o.0))
    }
}

impl std::ops::Add for Tick {
    type Output = Tick;
    fn add(self, o: Tick) -> Tick {
        Tick(self.0.saturating_add(o.0))
    }
}

/// Media time of a 16 kHz sample index.
pub fn sample_tick(n: i64) -> Tick {
    Tick(n.saturating_mul(TICKS_PER_SAMPLE))
}

/// Media time of `s` seconds (rounded to the nearest 16 kHz sample). Test helper.
#[cfg(test)]
pub fn seconds_tick(s: f64) -> Tick {
    sample_tick((s * SAMPLE_RATE as f64).round() as i64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sample_ticks_are_exact_and_saturate_instead_of_overflowing() {
        assert_eq!(sample_tick(0), Tick(0));
        assert_eq!(sample_tick(16_000).seconds(), 1.0);
        assert_eq!(sample_tick(-16_000).seconds(), -1.0);
        // far beyond any real position: pinned at the limit, never wrapped or panicking
        assert_eq!(sample_tick(i64::MAX), Tick(i64::MAX));
        assert_eq!(sample_tick(i64::MIN), Tick(i64::MIN));
        assert_eq!(sample_tick(i64::MAX / TICKS_PER_SAMPLE + 1), Tick(i64::MAX));
    }
}
