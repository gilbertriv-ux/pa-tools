//! Shared helpers for the integration tests: a deterministic PRNG, signal generators and
//! measurement functions. Each test binary uses a subset.
#![allow(dead_code)]

use soundcraft_fx::{AudioEffect, EffectInfo, ParamSpec, Unit};
use std::f64::consts::PI;

/// Sample rates every effect is exercised at.
pub const RATES: [f32; 7] = [8_000.0, 22_050.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0, 384_000.0];
/// The rates the behaviour tests run at.
pub const MUSIC_RATES: [f32; 3] = [44_100.0, 48_000.0, 96_000.0];

/// xorshift64* — deterministic, no dependencies.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [-1, 1).
    pub fn uniform(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f32 {
        (self.uniform() + 1.0) * 0.5
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

pub fn noise(rng: &mut Rng, n: usize, amp: f32) -> Vec<f32> {
    (0..n).map(|_| rng.uniform() * amp).collect()
}

pub fn sine(freq: f64, amp: f64, sr: f64, n: usize) -> Vec<f32> {
    (0..n).map(|i| (amp * (2.0 * PI * freq * i as f64 / sr).sin()) as f32).collect()
}

pub fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

pub fn db(x: f64) -> f64 {
    20.0 * x.max(1e-30).log10()
}

pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Amplitude of the `freq` component of `x` (single-bin correlation).
pub fn tone_amplitude(x: &[f32], freq: f64, sr: f64) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &v) in x.iter().enumerate() {
        let w = 2.0 * PI * freq * i as f64 / sr;
        re += v as f64 * w.cos();
        im += v as f64 * w.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len().max(1) as f64
}

/// Process planar buffers in place in blocks of `block` frames.
pub fn run(fx: &mut dyn AudioEffect, chans: &mut [Vec<f32>], block: usize) {
    let len = chans.iter().map(Vec::len).min().unwrap_or(0);
    let mut pos = 0;
    while pos < len {
        let n = block.max(1).min(len - pos);
        let mut views: Vec<&mut [f32]> = chans.iter_mut().map(|c| &mut c[pos..pos + n]).collect();
        fx.process(&mut views);
        pos += n;
    }
}

/// A value for a parameter: `0.0` = min, `1.0` = max.
pub fn at(p: &ParamSpec, t: f32) -> f32 {
    match p.unit {
        Unit::Choice | Unit::Toggle => (p.min + (p.max - p.min) * t).round(),
        _ => p.min + (p.max - p.min) * t,
    }
}

/// How every parameter is set in a sweep configuration.
#[derive(Clone, Copy, Debug)]
pub enum Setting {
    Default,
    Min,
    Max,
    Mid,
    Random(u64),
}

pub const SETTINGS: [Setting; 6] = [Setting::Default, Setting::Min, Setting::Max, Setting::Mid, Setting::Random(1), Setting::Random(2)];

pub fn apply(fx: &mut dyn AudioEffect, setting: Setting) {
    let mut rng = match setting {
        Setting::Random(s) => Rng::new(s),
        _ => Rng::new(0),
    };
    for p in fx.params() {
        let v = match setting {
            Setting::Default => p.default,
            Setting::Min => p.min,
            Setting::Max => p.max,
            Setting::Mid => at(p, 0.5),
            Setting::Random(_) => at(p, rng.unit()),
        };
        assert!(fx.set_param(p.id, v));
    }
}

pub fn make(info: &EffectInfo, sr: f32, ch: usize) -> Box<dyn AudioEffect> {
    (info.create)(sr, ch)
}

pub fn make_raw(info: &EffectInfo, sr: f32, ch: usize) -> Box<dyn AudioEffect> {
    (info.create_raw)(sr, ch)
}

/// Effects registered in Pa Tools first (see `docs/effects-donor.md`); the behaviour tests
/// concentrate on these. Ids are the final, neutral ones.
pub fn listed() -> Vec<&'static EffectInfo> {
    soundcraft_fx::effects().iter().collect()
}
