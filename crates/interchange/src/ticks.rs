//! Exact media time for the interchange documents.
//!
//! Inlined (and trimmed) from the donor's time crate, which is not a dependency here. All time in
//! a [`crate::comp::Document`] is an integer number of [`Tick`]s at [`TICKS_PER_SECOND`] =
//! 254 016 000 000/s. That rate divides evenly into every broadcast frame duration (23.976, 24, 25,
//! 29.97, 30, 48, 50, 59.94, 60, 120, ...) and every common audio sample duration (8 k to 192 k,
//! including the 44.1 k family), so conversions between frames, samples and edit units never drift.
//!
//! Every conversion is total: a zero or negative rate (from a damaged file) counts as the default
//! rate instead of dividing by zero, and arithmetic saturates instead of overflowing.

use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// Ticks per second.
pub const TICKS_PER_SECOND: i64 = 254_016_000_000;

/// A point or duration in time, in ticks. Arithmetic saturates at the `i64` limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tick(pub i64);

impl Tick {
    pub const ZERO: Tick = Tick(0);

    /// Exact conversion from a count of `units` at `per_second` (e.g. samples at 48 000).
    pub fn from_units(units: i64, per_second: i64) -> Tick {
        Tick(clamp_i64((i128::from(units) * i128::from(TICKS_PER_SECOND)).checked_div(i128::from(per_second)).unwrap_or(0)))
    }
    pub fn max(self, o: Tick) -> Tick {
        Tick(self.0.max(o.0))
    }
    /// Clamp to `lo..=hi` (never panics when the bounds cross, unlike `Ord::clamp`).
    pub fn clamp(self, lo: Tick, hi: Tick) -> Tick {
        Tick(self.0.max(lo.0).min(hi.0))
    }
}

impl Add for Tick {
    type Output = Tick;
    fn add(self, o: Tick) -> Tick {
        Tick(self.0.saturating_add(o.0))
    }
}
impl Sub for Tick {
    type Output = Tick;
    fn sub(self, o: Tick) -> Tick {
        Tick(self.0.saturating_sub(o.0))
    }
}
impl Neg for Tick {
    type Output = Tick;
    fn neg(self) -> Tick {
        Tick(self.0.saturating_neg())
    }
}
impl AddAssign for Tick {
    fn add_assign(&mut self, o: Tick) {
        self.0 = self.0.saturating_add(o.0);
    }
}
impl SubAssign for Tick {
    fn sub_assign(&mut self, o: Tick) {
        self.0 = self.0.saturating_sub(o.0);
    }
}

/// A frame rate as an exact rational (`num / den` frames per second).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameRate {
    pub num: i64,
    pub den: i64,
}

impl Default for FrameRate {
    fn default() -> Self {
        FrameRate::FPS_24
    }
}

impl FrameRate {
    pub const FPS_24: FrameRate = FrameRate { num: 24, den: 1 };
    pub const FPS_25: FrameRate = FrameRate { num: 25, den: 1 };

    /// The rate `num/den` in lowest terms (a zero denominator gives the default rate).
    pub fn new(num: i64, den: i64) -> Self {
        if den == 0 {
            return FrameRate::default();
        }
        let g = i128::from(gcd(num.unsigned_abs(), den.unsigned_abs()).max(1));
        FrameRate { num: clamp_i64(i128::from(num) / g), den: clamp_i64(i128::from(den) / g) }
    }

    /// This rate if it is positive, else the default.
    pub fn sane(self) -> FrameRate {
        if self.num > 0 && self.den > 0 { self } else { FrameRate::default() }
    }

    /// Index of the frame containing `t` (floor), saturated to the integer frame range.
    pub fn frame_at(self, t: Tick) -> i64 {
        let r = self.sane();
        clamp_i64((i128::from(t.0) * i128::from(r.num)).div_euclid(i128::from(TICKS_PER_SECOND) * i128::from(r.den)))
    }

    /// Start tick of frame `f` (ceiling division, so `frame_at(tick_of(f)) == f`).
    pub fn tick_of(self, f: i64) -> Tick {
        let r = self.sane();
        let n = i128::from(f) * i128::from(TICKS_PER_SECOND) * i128::from(r.den);
        let d = i128::from(r.num);
        Tick(clamp_i64(n.div_euclid(d) + i128::from(n.rem_euclid(d) != 0)))
    }

    /// Timecode base (frames counted per timecode second): 30 for 29.97, 24 for 23.976.
    pub fn timecode_base(self) -> i64 {
        let r = self.sane();
        (r.num / r.den).saturating_add(i64::from(r.num % r.den != 0)).max(1)
    }
}

/// Narrow only after applying the storage bounds, never by a wrapping integer cast.
fn clamp_i64(value: i128) -> i64 {
    i64::try_from(value.clamp(i128::from(i64::MIN), i128::from(i64::MAX))).unwrap_or(0)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_rates_are_exact_in_ticks() {
        for (n, d) in
            [(24_000, 1001), (24, 1), (25, 1), (30_000, 1001), (30, 1), (48, 1), (50, 1), (60_000, 1001), (60, 1), (120_000, 1001), (120, 1)]
        {
            let r = FrameRate::new(n, d);
            assert_eq!(i128::from(TICKS_PER_SECOND) * i128::from(r.den) % i128::from(r.num), 0, "{n}/{d}");
        }
        for sr in [8_000, 11_025, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 384_000] {
            assert_eq!(TICKS_PER_SECOND % sr, 0, "{sr}");
        }
    }

    #[test]
    fn frames_round_trip() {
        let r = FrameRate::new(24_000, 1001);
        for f in [-5, 0, 1, 23, 24, 1_000_000] {
            assert_eq!(r.frame_at(r.tick_of(f)), f);
        }
        assert_eq!(FrameRate::new(30_000, 1001).timecode_base(), 30);
        assert_eq!(FrameRate::new(24_000, 1001).timecode_base(), 24);
    }

    #[test]
    fn damaged_rates_never_panic() {
        for (n, d) in [(0, 0), (0, 1), (1, 0), (-1, 1), (i64::MIN, -1), (i64::MAX, i64::MAX), (1, i64::MIN)] {
            let r = FrameRate::new(n, d);
            let _ = (r.frame_at(Tick(i64::MAX)), r.frame_at(Tick(i64::MIN)), r.tick_of(i64::MAX), r.tick_of(i64::MIN), r.timecode_base());
        }
        assert_eq!(Tick::from_units(5, 0), Tick::ZERO);
        assert_eq!(Tick(i64::MAX) + Tick(1), Tick(i64::MAX));
        assert_eq!(Tick(5).clamp(Tick(9), Tick(1)), Tick(1));
    }
}
