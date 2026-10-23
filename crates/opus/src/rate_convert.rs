//! Streaming sample-rate conversion to 48 kHz for the Opus writer (Kaiser-windowed sinc).
//!
//! The Opus encoder takes 48 kHz audio only. This converter sits in front of it for every other input
//! rate. It has no latency: output sample `n` is the band-limited signal read at input time
//! `n * in_rate / out_rate`, computed with exact integer arithmetic, so the first output sample lines
//! up with the first input sample and nothing drifts over a long file. Feeding the input in chunks of
//! any size gives bit-identical output.
//!
//! The layer rules keep this a private copy of the idea in `soundcraft-dsp`'s offline resampler: that
//! crate is L1 and this one is L0.

/// Zero crossings of the sinc on each side of the centre.
const ZERO_CROSSINGS: f64 = 24.0;
/// Kernel table entries per input-sample interval (linearly interpolated between entries).
const TABLE_RES: usize = 512;
/// Kaiser window shape (about 90 dB of stop-band rejection).
const KAISER_BETA: f64 = 9.0;
/// Pass band edge as a fraction of the lower of the two Nyquist frequencies.
const BAND_EDGE: f64 = 0.97;
/// Input frames dropped from the front of the buffer at a time.
const COMPACT_AT: usize = 1 << 14;

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0f64, 1.0f64);
    let y = x * x / 4.0;
    for k in 1..64 {
        term *= y / (f64::from(k) * f64::from(k));
        sum += term;
        if term < sum * 1e-14 {
            break;
        }
    }
    sum
}

/// Rate converter from `in_rate` to `out_rate` for `channels` interleaved channels.
pub(crate) struct Resampler {
    channels: usize,
    in_rate: u64,
    out_rate: u64,
    /// One-sided kernel `h(i / TABLE_RES)`, zero past the half width.
    table: Vec<f32>,
    /// Taps on each side of the centre, in input samples.
    half: i64,
    /// Planar input history; `bufs[c][0]` is absolute input index `base`.
    bufs: Vec<Vec<f32>>,
    base: i64,
    /// Outputs produced so far.
    next_out: u64,
    weights: Vec<f32>,
}

impl Resampler {
    /// `None` for a zero channel count or rate.
    pub(crate) fn new(channels: usize, in_rate: u32, out_rate: u32) -> Option<Resampler> {
        if channels == 0 || in_rate == 0 || out_rate == 0 {
            return None;
        }
        // Low-pass at 97 % of the lower Nyquist, in cycles per input sample.
        let fc = 0.5 * BAND_EDGE * (f64::from(out_rate) / f64::from(in_rate)).min(1.0);
        let half_width = ZERO_CROSSINGS / (2.0 * fc);
        let half = half_width.ceil() as i64;
        let i0 = bessel_i0(KAISER_BETA);
        let entries = (half as usize) * TABLE_RES + 2;
        let table = (0..entries)
            .map(|i| {
                let x = i as f64 / TABLE_RES as f64;
                let r = x / half_width;
                if r >= 1.0 {
                    return 0.0;
                }
                let u = 2.0 * fc * x;
                let sinc = if u.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u) };
                (2.0 * fc * sinc * bessel_i0(KAISER_BETA * (1.0 - r * r).sqrt()) / i0) as f32
            })
            .collect();
        // The history starts with `half` zero samples standing in for the signal before time zero.
        let bufs = vec![vec![0.0; half as usize]; channels];
        Some(Resampler {
            channels,
            in_rate: u64::from(in_rate),
            out_rate: u64::from(out_rate),
            table,
            half,
            bufs,
            base: -half,
            next_out: 0,
            weights: vec![0.0; 2 * half as usize],
        })
    }

    /// Output frames for `input_frames` input frames: `round(input_frames * out_rate / in_rate)`.
    pub(crate) fn output_len(&self, input_frames: u64) -> u64 {
        let num = u128::from(input_frames) * u128::from(self.out_rate) + u128::from(self.in_rate) / 2;
        u64::try_from(num / u128::from(self.in_rate)).unwrap_or(u64::MAX)
    }

    /// Converts `input` (interleaved, a whole number of frames; a trailing partial frame is ignored)
    /// and appends every output frame that has all the input it needs to `out`, up to `limit` outputs
    /// in total.
    pub(crate) fn process(&mut self, input: &[f32], limit: u64, out: &mut Vec<f32>) {
        let c = self.channels;
        for frame in input.chunks_exact(c) {
            for (buf, &v) in self.bufs.iter_mut().zip(frame) {
                buf.push(v);
            }
        }
        let have = self.bufs.first().map_or(0, Vec::len) as i64;
        while self.next_out < limit {
            let pos = u128::from(self.next_out) * u128::from(self.in_rate);
            let int = (pos / u128::from(self.out_rate)) as i64;
            if int + self.half >= self.base + have {
                break;
            }
            let frac = (pos % u128::from(self.out_rate)) as f64 / self.out_rate as f64;
            self.fill_weights(frac);
            let start = (int - self.half + 1 - self.base) as usize;
            for buf in &self.bufs {
                let acc = buf.get(start..start + self.weights.len()).map_or(0.0, |w| w.iter().zip(&self.weights).map(|(a, b)| a * b).sum());
                out.push(acc);
            }
            self.next_out += 1;
        }
        self.compact();
    }

    /// Ends the stream: treats the signal as silent after the last input frame and appends the
    /// remaining outputs up to `total` outputs overall.
    pub(crate) fn finish(&mut self, total: u64, out: &mut Vec<f32>) {
        let silence = vec![0.0; (self.half as usize + 2) * self.channels];
        // One round of silence is enough lookahead for the final output; loop in case of a long gap.
        let mut guard = 0;
        while self.next_out < total && guard < 4 {
            self.process(&silence, total, out);
            guard += 1;
        }
    }

    /// Kernel weights for the taps around an output at fractional input position `frac`.
    fn fill_weights(&mut self, frac: f64) {
        let first = 1 - self.half;
        for (k, w) in self.weights.iter_mut().enumerate() {
            let x = ((first + k as i64) as f64 - frac).abs() * TABLE_RES as f64;
            let i = x as usize;
            let f = (x - i as f64) as f32;
            *w = match (self.table.get(i), self.table.get(i + 1)) {
                (Some(&a), Some(&b)) => a + (b - a) * f,
                _ => 0.0,
            };
        }
    }

    /// Drops input no later output can use.
    fn compact(&mut self) {
        let pos = u128::from(self.next_out) * u128::from(self.in_rate);
        let int = (pos / u128::from(self.out_rate)) as i64;
        let drop = (int - self.half + 1 - self.base).max(0) as usize;
        if drop >= COMPACT_AT {
            for buf in &mut self.bufs {
                buf.drain(..drop.min(buf.len()));
            }
            self.base += drop as i64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, frames: usize, amp: f64) -> Vec<f32> {
        (0..frames).map(|n| (amp * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(rate)).sin()) as f32).collect()
    }

    fn convert(x: &[f32], channels: usize, from: u32, to: u32, chunk: usize) -> Vec<f32> {
        let mut r = Resampler::new(channels, from, to).unwrap();
        let frames = (x.len() / channels) as u64;
        let total = r.output_len(frames);
        let mut out = Vec::new();
        for c in x.chunks(chunk * channels) {
            r.process(c, total, &mut out);
        }
        r.finish(total, &mut out);
        assert_eq!(out.len() as u64, total * channels as u64);
        out
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    #[test]
    fn refuses_zero_arguments() {
        assert!(Resampler::new(0, 44_100, 48_000).is_none());
        assert!(Resampler::new(1, 0, 48_000).is_none());
        assert!(Resampler::new(1, 44_100, 0).is_none());
    }

    #[test]
    fn output_length_is_the_rounded_ratio() {
        let r = Resampler::new(1, 44_100, 48_000).unwrap();
        assert_eq!(r.output_len(0), 0);
        assert_eq!(r.output_len(44_100), 48_000);
        assert_eq!(r.output_len(1), 1);
        let r = Resampler::new(1, 192_000, 48_000).unwrap();
        assert_eq!(r.output_len(3), 1);
        assert_eq!(r.output_len(u64::MAX), u64::MAX / 4 + 1);
    }

    #[test]
    fn a_sine_keeps_its_level_phase_and_frequency() {
        for from in [44_100u32, 32_000, 22_050, 96_000, 88_200, 8_000] {
            let x = sine(from, 1_000.0, from as usize, 0.5);
            let y = convert(&x, 1, from, 48_000, 4096);
            let reference = sine(48_000, 1_000.0, y.len(), 0.5);
            // ignore the edges, where the kernel sees the start and end of the signal
            let (a, b) = (3_000, y.len() - 3_000);
            let err: Vec<f32> = y[a..b].iter().zip(&reference[a..b]).map(|(p, q)| p - q).collect();
            let snr = 20.0 * (rms(&reference[a..b]) / rms(&err)).log10();
            assert!(snr > 70.0, "{from} Hz -> 48 kHz: SNR {snr:.1} dB");
        }
    }

    #[test]
    fn the_first_sample_lines_up_with_the_first_input_sample() {
        // an impulse at input 100 comes out at output 100 * 48000 / 44100 (no latency)
        let mut x = vec![0.0f32; 1000];
        x[100] = 1.0;
        let y = convert(&x, 1, 44_100, 48_000, 1000);
        let peak = y.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i).unwrap();
        assert_eq!(peak, (100.0f64 * 48_000.0 / 44_100.0).round() as usize);
    }

    #[test]
    fn content_above_the_new_nyquist_is_removed() {
        let x = sine(96_000, 30_000.0, 96_000, 0.9);
        let y = convert(&x, 1, 96_000, 48_000, 4096);
        assert!(rms(&y[2_000..y.len() - 2_000]) < 0.9 * 10f64.powf(-80.0 / 20.0), "30 kHz leaked: {}", rms(&y));
    }

    #[test]
    fn chunk_size_does_not_change_the_output() {
        let x: Vec<f32> = (0..20_000).map(|n| ((n * 7919) % 211) as f32 / 211.0 - 0.5).collect();
        let whole = convert(&x, 2, 44_100, 48_000, 10_000);
        for chunk in [1usize, 7, 480, 4097] {
            assert_eq!(convert(&x, 2, 44_100, 48_000, chunk), whole, "chunk {chunk}");
        }
    }

    #[test]
    fn stereo_channels_stay_separate() {
        let l = sine(44_100, 440.0, 20_000, 0.5);
        let x: Vec<f32> = l.iter().flat_map(|v| [*v, 0.0]).collect();
        let y = convert(&x, 2, 44_100, 48_000, 4096);
        assert!(y.iter().skip(1).step_by(2).all(|v| *v == 0.0));
        assert!(rms(&y.iter().step_by(2).copied().collect::<Vec<_>>()) > 0.3);
    }

    #[test]
    fn tiny_inputs_produce_the_right_count() {
        for frames in [1usize, 2, 5, 100] {
            let y = convert(&vec![0.25; frames], 1, 44_100, 48_000, 3);
            assert_eq!(y.len() as u64, Resampler::new(1, 44_100, 48_000).unwrap().output_len(frames as u64));
        }
    }
}
