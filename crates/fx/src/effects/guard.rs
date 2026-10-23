//! The safety wrapper every registered effect runs inside.
//!
//! Effects are written for finite audio and in-range parameters. Hosts cannot promise either:
//! a corrupt file, a clipped converter or an unstable plug-in upstream can hand over NaN, ±∞ or
//! absurdly large samples, and a recursive filter that has taken in one NaN never recovers. The
//! wrapper makes the boundary safe without touching the effect:
//!
//! * input samples that are NaN or ±∞ become 0 and finite ones are clamped to
//!   ±[`INPUT_LIMIT`], so no effect ever sees a non-finite value;
//! * if an output sample is non-finite or at least [`OUTPUT_LIMIT`] in magnitude (a blown-up
//!   state), the effect is reset and the block is silenced, so the damage lasts one block;
//! * subnormal samples are flushed to zero on the way in and out, so slow denormal arithmetic
//!   cannot start in or leak out of an effect.
//!
//! It adds one pass over each block and never allocates.

use crate::{AudioEffect, ParamSpec, block_len};

/// Largest input magnitude handed to an effect (+120 dBFS); larger finite samples are clamped.
pub const INPUT_LIMIT: f32 = 1.0e6;
/// An output sample at or beyond this magnitude counts as a blow-up.
pub const OUTPUT_LIMIT: f32 = 1.0e9;

#[inline(always)]
fn clean_input(v: f32) -> f32 {
    let a = v.abs();
    if a <= INPUT_LIMIT {
        if a < f32::MIN_POSITIVE && a != 0.0 { 0.0 } else { v }
    } else if a.is_infinite() || v.is_nan() {
        0.0
    } else {
        INPUT_LIMIT.copysign(v)
    }
}

/// An effect wrapped in the input/output guard described in the [module docs](self).
pub struct Guarded<E: AudioEffect> {
    inner: E,
    faults: u32,
}

impl<E: AudioEffect> Guarded<E> {
    pub fn new(inner: E) -> Self {
        Guarded { inner, faults: 0 }
    }

    /// How many times the output blew up and the effect was reset.
    pub fn faults(&self) -> u32 {
        self.faults
    }

    /// The wrapped effect.
    pub fn inner(&self) -> &E {
        &self.inner
    }
}

impl<E: AudioEffect> AudioEffect for Guarded<E> {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn params(&self) -> &'static [ParamSpec] {
        self.inner.params()
    }
    fn set_param(&mut self, id: &str, value: f32) -> bool {
        self.inner.set_param(id, value)
    }
    fn param(&self, id: &str) -> Option<f32> {
        self.inner.param(id)
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn latency(&self) -> usize {
        self.inner.latency()
    }
    fn tail_samples(&self) -> usize {
        self.inner.tail_samples()
    }
    fn response_db(&self, freq: f64) -> Option<f64> {
        self.inner.response_db(freq)
    }
    fn transfer_db(&self, band: usize, input_db: f32) -> Option<f32> {
        self.inner.transfer_db(band, input_db)
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        if n == 0 {
            self.inner.process(channels);
            return;
        }
        for ch in channels.iter_mut() {
            for v in ch.iter_mut().take(n) {
                *v = clean_input(*v);
            }
        }
        self.inner.process(channels);
        let mut blown = false;
        'scan: for ch in channels.iter_mut() {
            for v in ch.iter_mut().take(n) {
                let a = v.abs();
                if a.is_nan() || a >= OUTPUT_LIMIT {
                    blown = true;
                    break 'scan;
                }
                if a < f32::MIN_POSITIVE && a != 0.0 {
                    *v = 0.0;
                }
            }
        }
        if blown {
            self.faults = self.faults.saturating_add(1);
            self.inner.reset();
            for ch in channels.iter_mut() {
                ch.iter_mut().take(n).for_each(|v| *v = 0.0);
            }
        }
    }
}
