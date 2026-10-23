//! Every effect, at every supported sample rate, under extreme and random parameters: the output
//! must stay finite and bounded, tails must decay without denormals, and latency must not move.

mod common;
use common::*;
use std::time::Instant;

/// Output magnitude that no sane setting reaches (+400 dBFS); a stable effect never gets near
/// it, an unstable one passes it long before f32 overflows.
const RUNAWAY: f32 = 1.0e20;

fn sweep_one(info: &soundcraft_fx::EffectInfo, sr: f32, ch: usize, setting: Setting, seed: u64, guarded: bool, fails: &mut Vec<String>) {
    let tag = format!("{} @ {sr} Hz x{ch} {setting:?}{}", info.id, if guarded { " guarded" } else { "" });
    let mut fx = if guarded { make(info, sr, ch) } else { make_raw(info, sr, ch) };
    apply(fx.as_mut(), setting);
    let latency = fx.latency();
    let mut rng = Rng::new(seed);
    // 120 ms of loud noise with isolated spikes, then 0.6 s of silence.
    let burst = (sr * 0.12) as usize;
    let tail = (sr * 0.6) as usize;
    let mut chans: Vec<Vec<f32>> = (0..ch)
        .map(|c| {
            let mut v = noise(&mut rng, burst, 0.9);
            v[burst / 3] = 8.0 * if c % 2 == 0 { 1.0 } else { -1.0 };
            v.extend(std::iter::repeat_n(0.0, tail));
            v
        })
        .collect();
    run(fx.as_mut(), &mut chans, 509);
    for (c, x) in chans.iter().enumerate() {
        let bad = x.iter().position(|v| !v.is_finite());
        if bad.is_some() {
            fails.push(format!("{tag}: ch{c} non-finite at {bad:?}"));
            return;
        }
        let pk = peak(x);
        if pk >= RUNAWAY {
            fails.push(format!("{tag}: ch{c} peak {pk}"));
            return;
        }
    }
    if fx.latency() != latency {
        fails.push(format!("{tag}: latency changed while running"));
    }
    if fx.tail_samples() > (soundcraft_fx::MAX_TAIL_SECONDS * sr) as usize + 1 {
        fails.push(format!("{tag}: tail too long"));
    }
    // Long silence: still finite and no denormals leak out.
    let mut quiet: Vec<Vec<f32>> = vec![vec![0.0; (sr * 0.3) as usize]; ch];
    run(fx.as_mut(), &mut quiet, 4096);
    for (c, x) in quiet.iter().enumerate() {
        if !x.iter().all(|v| v.is_finite()) {
            fails.push(format!("{tag}: ch{c} NaN/inf in tail"));
            return;
        }
        if guarded && !x.iter().all(|v| !v.is_subnormal()) {
            fails.push(format!("{tag}: ch{c} denormal in tail"));
            return;
        }
    }
    // Reset leaves a clean state: silence in, silence out (generators excepted: none here).
    fx.reset();
    let mut z: Vec<Vec<f32>> = vec![vec![0.0; 2048]; ch];
    run(fx.as_mut(), &mut z, 512);
    let leftover = z.iter().map(|c| peak(c)).fold(0.0f32, f32::max);
    if leftover >= 1e-3 {
        fails.push(format!("{tag}: reset did not clear state ({leftover})"));
    }
}

#[test]
fn all_effects_all_rates_stereo() {
    let t0 = Instant::now();
    let mut fails = Vec::new();
    for info in listed() {
        for &sr in &RATES {
            for (k, &setting) in SETTINGS.iter().enumerate() {
                sweep_one(info, sr, 2, setting, 100 + k as u64, false, &mut fails);
            }
        }
    }
    eprintln!("sweep stereo: {:?}", t0.elapsed());
    assert!(fails.is_empty(), "{} failures:\n{}", fails.len(), fails.join("\n"));
}

#[test]
fn all_effects_mono_and_surround() {
    let mut fails = Vec::new();
    for info in listed() {
        for &sr in &[8_000.0, 48_000.0, 384_000.0] {
            for (k, &setting) in [Setting::Default, Setting::Max, Setting::Random(7)].iter().enumerate() {
                sweep_one(info, sr, 1, setting, 200 + k as u64, false, &mut fails);
                sweep_one(info, sr, 6, setting, 300 + k as u64, false, &mut fails);
            }
        }
    }
    assert!(fails.is_empty(), "{} failures:\n{}", fails.len(), fails.join("\n"));
}

/// The wrapped effects (what hosts get) are denormal-free in their tails at every rate.
#[test]
fn guarded_effects_have_clean_tails() {
    let mut fails = Vec::new();
    for info in listed() {
        for &sr in &RATES {
            for (k, &setting) in [Setting::Random(1), Setting::Random(2), Setting::Max].iter().enumerate() {
                sweep_one(info, sr, 2, setting, 400 + k as u64, true, &mut fails);
            }
        }
    }
    assert!(fails.is_empty(), "{} failures:\n{}", fails.len(), fails.join("\n"));
}
