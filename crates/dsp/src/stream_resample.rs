//! Bounded sample-rate conversion without retaining an entire source.
//!
//! [`resample_windowed`] is a worker-only Kaiser-windowed sinc converter, using the same
//! antialias kernel as the offline converter. Its callbacks read source samples and immediately
//! consume interleaved output frames; its bounded table and scratch do not grow with duration.
//! [`StreamResampler`] is a cheaper chunk-fed linear converter for monitoring. Linear conversion
//! has no low-pass antialias filter and must not silently replace high-quality import SRC.

use std::fmt;

pub const MAX_CHANNELS: usize = 256;
pub const MAX_INPUT_FRAMES: usize = 16_384;
const MAX_RATE: u32 = 1_536_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResampleError {
    InvalidFormat,
    InvalidBlock,
    Finished,
    PositionOverflow,
}
impl fmt::Display for ResampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidFormat => "invalid streaming resampler rate or channel count",
            Self::InvalidBlock => "streaming input must have equal, bounded channel lengths",
            Self::Finished => "streaming resampler is finished or its output failed",
            Self::PositionOverflow => "streaming resampler position overflow",
        })
    }
}
impl std::error::Error for ResampleError {}

#[derive(Debug)]
pub enum StreamError<E> {
    Resample(ResampleError),
    Output(E),
}
impl<E: fmt::Display> fmt::Display for StreamError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resample(e) => fmt::Display::fmt(e, f),
            Self::Output(e) => write!(f, "resampler output: {e}"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for StreamError<E> {}

/// Rational input position is `next / to`, advancing by `from` per output frame. With N input
/// frames, finishing emits exactly ceil(N * to / from) frames and clamps the final interval.
/// An output-sink failure poisons the stream; resume would otherwise duplicate an output frame.
pub struct StreamResampler {
    from: u32,
    to: u32,
    previous: Vec<f32>,
    output: Vec<f32>,
    seen: u64,
    next: u128,
    finished: bool,
}
impl StreamResampler {
    pub fn new(from: u32, to: u32, channels: usize) -> Result<Self, ResampleError> {
        if from == 0 || to == 0 || from > MAX_RATE || to > MAX_RATE || !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ResampleError::InvalidFormat);
        }
        Ok(Self { from, to, previous: vec![0.0; channels], output: vec![0.0; channels], seen: 0, next: 0, finished: false })
    }

    pub fn input_frames(&self) -> u64 {
        self.seen
    }

    /// Approximate retained sample-buffer bytes (excluding the small fixed struct itself).
    pub fn retained_sample_bytes(&self) -> usize {
        (self.previous.capacity() + self.output.capacity()).saturating_mul(std::mem::size_of::<f32>())
    }

    pub fn try_process<E>(&mut self, input: &[Vec<f32>], end: bool, mut emit: impl FnMut(&[f32]) -> Result<(), E>) -> Result<(), StreamError<E>> {
        if self.finished {
            return Err(StreamError::Resample(ResampleError::Finished));
        }
        let frames = input.first().map_or(0, Vec::len);
        if input.len() != self.previous.len() || frames > MAX_INPUT_FRAMES || input.iter().any(|c| c.len() != frames) {
            return Err(StreamError::Resample(ResampleError::InvalidBlock));
        }
        // Validate position before consuming any frame from this block.
        if self.seen.checked_add(frames as u64).is_none() {
            return Err(StreamError::Resample(ResampleError::PositionOverflow));
        }
        for frame in 0..frames {
            let current = u128::from(self.seen) * u128::from(self.to);
            let previous = current.saturating_sub(u128::from(self.to));
            while self.next <= current {
                let fraction = if self.seen == 0 { 1.0 } else { (self.next.saturating_sub(previous) as f64 / f64::from(self.to)) as f32 };
                for ((out, old), data) in self.output.iter_mut().zip(&self.previous).zip(input) {
                    let new = data.get(frame).copied().unwrap_or(0.0);
                    let new = if new.is_finite() { new } else { 0.0 };
                    *out =
                        if self.next == current { new } else { (f64::from(*old) + (f64::from(new) - f64::from(*old)) * f64::from(fraction)) as f32 };
                }
                if let Err(e) = emit(&self.output) {
                    self.finished = true;
                    return Err(StreamError::Output(e));
                }
                self.next += u128::from(self.from);
            }
            for (old, data) in self.previous.iter_mut().zip(input) {
                let value = data.get(frame).copied().unwrap_or(0.0);
                *old = if value.is_finite() { value } else { 0.0 };
            }
            self.seen += 1;
        }
        if end {
            let limit = u128::from(self.seen) * u128::from(self.to);
            while self.next < limit {
                if let Err(e) = emit(&self.previous) {
                    self.finished = true;
                    return Err(StreamError::Output(e));
                }
                self.next += u128::from(self.from);
            }
            self.finished = true;
        }
        Ok(())
    }
}

/// A worker-side input or output failure. Already-created outputs must be retained by callers.
#[derive(Debug)]
pub enum WindowedError<E> {
    Resample(ResampleError),
    Input(E),
    Output(E),
}
impl<E: fmt::Display> fmt::Display for WindowedError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resample(e) => fmt::Display::fmt(e, f),
            Self::Input(e) => write!(f, "resampler source: {e}"),
            Self::Output(e) => write!(f, "resampler output: {e}"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for WindowedError<E> {}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    let y = x * x / 4.0;
    for k in 1..64 {
        term *= y / (f64::from(k) * f64::from(k));
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

const SINC_RESOLUTION: usize = 512;
struct WindowedKernel {
    table: Vec<f32>,
    radius: u64,
}
impl WindowedKernel {
    fn new(ratio: f64) -> Self {
        let cutoff = (if ratio < 1.0 { ratio * 0.94 } else { 0.97 }).clamp(0.01, 1.0);
        let half_width = 32.0 / cutoff;
        let length = (half_width * SINC_RESOLUTION as f64).ceil() as usize + 2;
        let normalizer = bessel_i0(9.0);
        let table = (0..length)
            .map(|i| {
                let distance = i as f64 / SINC_RESOLUTION as f64;
                let window_position = distance / half_width;
                if window_position >= 1.0 {
                    return 0.0;
                }
                let angle = std::f64::consts::PI * cutoff * distance;
                let sinc = if angle.abs() < 1e-12 { 1.0 } else { angle.sin() / angle };
                let window = bessel_i0(9.0 * (1.0 - window_position * window_position).max(0.0).sqrt()) / normalizer;
                (cutoff * sinc * window) as f32
            })
            .collect();
        Self { table, radius: half_width.ceil() as u64 }
    }
    fn at(&self, distance: f64) -> f32 {
        let position = distance.abs() * SINC_RESOLUTION as f64;
        let index = position as usize;
        let fraction = (position - index as f64) as f32;
        match (self.table.get(index), self.table.get(index.saturating_add(1))) {
            (Some(a), Some(b)) => a + (b - a) * fraction,
            (Some(a), None) => *a,
            _ => 0.0,
        }
    }
}

/// Convert a source using a bounded Kaiser-windowed sinc table and one reused output frame.
///
/// Run on a background/control worker, never an audio callback: `read` may seek or wait. The
/// source is zero-padded outside `[0, total_frames)` and output length is nearest-rounded,
/// matching [`crate::offline::resample`]. Integer quotient/remainder keeps source positions
/// accurate even beyond f64's integer precision. File-backed readers should cache bounded
/// neighboring blocks so successive sample callbacks do not issue a file operation per tap.
///
/// Maximum table memory is about 6.3 MiB (the same 0.01 minimum-cutoff policy as offline SRC),
/// plus at most 256 output samples. No allocation scales with source duration. The caller
/// accounts this working set in its background-worker budget. Equal-rate input is passed
/// through exactly apart from non-finite values, which become silence.
pub fn resample_windowed<E>(
    from: u32,
    to: u32,
    channels: usize,
    total_frames: u64,
    mut read: impl FnMut(usize, u64) -> Result<f32, E>,
    mut emit: impl FnMut(&[f32]) -> Result<(), E>,
) -> Result<u64, WindowedError<E>> {
    if from == 0 || to == 0 || from > MAX_RATE || to > MAX_RATE || !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(WindowedError::Resample(ResampleError::InvalidFormat));
    }
    let output_frames = (u128::from(total_frames) * u128::from(to) + u128::from(from / 2)) / u128::from(from);
    let output_frames = u64::try_from(output_frames).map_err(|_| WindowedError::Resample(ResampleError::PositionOverflow))?;
    if output_frames == 0 {
        return Ok(0);
    }
    let mut output = vec![0.0f32; channels];
    if from == to {
        for frame in 0..total_frames {
            for (channel, value) in output.iter_mut().enumerate() {
                let source = read(channel, frame).map_err(WindowedError::Input)?;
                *value = if source.is_finite() { source } else { 0.0 };
            }
            emit(&output).map_err(WindowedError::Output)?;
        }
        return Ok(output_frames);
    }
    let kernel = WindowedKernel::new(f64::from(to) / f64::from(from));
    for frame in 0..output_frames {
        let position = u128::from(frame) * u128::from(from);
        let integral = u64::try_from(position / u128::from(to)).map_err(|_| WindowedError::Resample(ResampleError::PositionOverflow))?;
        let fraction = (position % u128::from(to)) as f64 / f64::from(to);
        let first = integral.saturating_sub(kernel.radius);
        let last = integral.saturating_add(kernel.radius).min(total_frames.saturating_sub(1));
        for (channel, value) in output.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            for source_frame in first..=last {
                let distance =
                    if source_frame <= integral { (integral - source_frame) as f64 + fraction } else { fraction - (source_frame - integral) as f64 };
                let weight = kernel.at(distance);
                if weight == 0.0 {
                    continue;
                }
                let source = read(channel, source_frame).map_err(WindowedError::Input)?;
                if source.is_finite() {
                    sum += source * weight;
                }
            }
            *value = if sum.is_finite() { sum } else { 0.0 };
        }
        emit(&output).map_err(WindowedError::Output)?;
    }
    Ok(output_frames)
}
