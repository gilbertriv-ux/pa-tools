//! Multi-resolution overview for waveform drawing.
//!
//! Level 0 sums up each block of [`BLOCK`] samples (min, max and sum of squares); each following
//! level merges [`FANOUT`] blocks of the level below, until a level has a single block. Range
//! queries combine the coarsest whole blocks that fit inside the range and read the partial blocks
//! at its edges from the samples, so a query is exact and costs O(log n) regardless of zoom.

/// Samples per level-0 block.
pub const BLOCK: usize = 32;
/// Blocks of level `n` merged into one block of level `n + 1`.
pub const FANOUT: usize = 4;
/// Upper bound on the number of spans [`Peaks::spans`] will produce.
pub const MAX_COLUMNS: usize = 1 << 20;

/// Multi-resolution overview of one channel for waveform drawing.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Peaks {
    len: usize,
    levels: Vec<Vec<Block>>,
}

/// What a run of samples sums up to; NaN samples count for nothing.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
struct Block {
    min: f32,
    max: f32,
    squares: f32,
}

impl Block {
    const EMPTY: Block = Block { min: f32::INFINITY, max: f32::NEG_INFINITY, squares: 0.0 };

    fn point(s: f32) -> Block {
        Block { min: s, max: s, squares: if s.is_finite() { s * s } else { 0.0 } }
    }

    fn of(samples: &[f32]) -> Block {
        samples.iter().fold(Block::EMPTY, |b, &s| b.merge(Block::point(s)))
    }

    fn merge(self, o: Block) -> Block {
        Block { min: self.min.min(o.min), max: self.max.max(o.max), squares: self.squares + o.squares }
    }

    /// (min, max), or (0, 0) when nothing was summed up.
    fn range(self) -> (f32, f32) {
        if self.min > self.max { (0.0, 0.0) } else { (self.min, self.max) }
    }
}

impl Peaks {
    /// Build the overview of `samples` (one channel). NaN samples are ignored.
    pub fn build(samples: &[f32]) -> Peaks {
        if samples.is_empty() {
            return Peaks::default();
        }
        let mut levels: Vec<Vec<Block>> = vec![samples.chunks(BLOCK).map(Block::of).collect()];
        while let Some(last) = levels.last().filter(|l| l.len() > 1) {
            let next = last.chunks(FANOUT).map(|c| c.iter().fold(Block::EMPTY, |a, &b| a.merge(b))).collect();
            levels.push(next);
        }
        Peaks { len: samples.len(), levels }
    }

    /// Number of samples the overview was built from.
    pub fn len_samples(&self) -> usize {
        self.len
    }

    /// Number of resolution levels (0 for an empty overview).
    pub fn num_levels(&self) -> usize {
        self.levels.len()
    }

    /// (min, max) over the sample range `[start, end)`, clamped to the data. `samples` must be the
    /// data the overview was built from. Returns `(0.0, 0.0)` for an empty range.
    pub fn minmax(&self, samples: &[f32], start: usize, end: usize) -> (f32, f32) {
        self.sum(samples, start, end).range()
    }

    fn sum(&self, samples: &[f32], start: usize, end: usize) -> Block {
        let end = end.min(self.len).min(samples.len());
        let (first, last) = (start.div_ceil(BLOCK), end / BLOCK);
        let raw = |a: usize, b: usize| Block::of(samples.get(a..b).unwrap_or_default());
        if first >= last {
            return raw(start, end);
        }
        raw(start, first * BLOCK).merge(raw(last * BLOCK, end)).merge(self.blocks(first, last))
    }

    /// The level-0 blocks `[lo, hi)`, summed from the coarsest blocks that fit.
    fn blocks(&self, mut lo: usize, mut hi: usize) -> Block {
        let mut acc = Block::EMPTY;
        for (lvl, level) in self.levels.iter().enumerate() {
            let mut add = |i: usize| acc = acc.merge(level.get(i).copied().unwrap_or(Block::EMPTY));
            if lvl + 1 == self.levels.len() || hi.saturating_sub(lo) < 2 * FANOUT {
                (lo..hi).for_each(add);
                break;
            }
            while !lo.is_multiple_of(FANOUT) {
                add(lo);
                lo += 1;
            }
            while !hi.is_multiple_of(FANOUT) {
                hi -= 1;
                add(hi);
            }
            lo /= FANOUT;
            hi /= FANOUT;
        }
        acc
    }

    /// The spans of sample positions `[start + c * step, start + (c + 1) * step + overlap]` that
    /// reach the data, clamped to it.
    fn windows(&self, samples: &[f32], start: f64, step: f64, overlap: f64, cols: usize) -> impl Iterator<Item = Option<(f64, f64)>> {
        self.windows_n(self.len.min(samples.len()), start, step, overlap, cols)
    }

    /// [`Peaks::windows`] against an explicit sample count `n` (so the overview-only methods can run
    /// without the samples themselves).
    fn windows_n(&self, n: usize, start: f64, step: f64, overlap: f64, cols: usize) -> impl Iterator<Item = Option<(f64, f64)>> {
        let valid = n > 0 && start.is_finite() && step.is_finite() && step > 0.0 && overlap.is_finite() && overlap >= 0.0;
        let last = n.saturating_sub(1) as f64;
        (0..cols.min(MAX_COLUMNS)).map(move |c| {
            let (a, b) = (start + step * c as f64, start + step * (c + 1) as f64 + overlap);
            (valid && b >= 0.0 && a <= last).then(|| (a.max(0.0), b.min(last)))
        })
    }

    /// (min, max) over `[start, end)` from the overview alone (no samples). The ragged edges are
    /// rounded out to whole level-0 blocks, so it is coarser than [`Peaks::minmax`] by at most
    /// [`BLOCK`] samples on each side — invisible at the zoomed-out levels a streamed source draws
    /// its waveform at, where no resident samples are held to refine the edges.
    pub fn minmax_overview(&self, start: usize, end: usize) -> (f32, f32) {
        self.sum_pyramid(start, end).range()
    }

    /// [`Peaks::sum`] from the overview alone: whole level-0 blocks covering `[start, end)`.
    fn sum_pyramid(&self, start: usize, end: usize) -> Block {
        let end = end.min(self.len);
        if start >= end {
            return Block::EMPTY;
        }
        let first = start / BLOCK;
        let last = (end - 1) / BLOCK + 1;
        self.blocks(first, last)
    }

    /// [`Peaks::spans`] from the overview alone (no samples), for streamed sources. Fractional
    /// span ends round out to whole samples; the per-sample interpolation `spans` does at its ends
    /// is dropped (there are no resident samples to interpolate between).
    pub fn spans_overview(&self, start: f64, step: f64, overlap: f64, cols: usize) -> Vec<Option<(f32, f32)>> {
        self.windows_n(self.len, start, step, overlap, cols)
            .map(|w| w.map(|(a, b)| self.sum_pyramid(a.floor() as usize, (b.ceil() as usize).saturating_add(1)).range()))
            .collect()
    }

    /// [`Peaks::rms_spans`] from the overview alone (no samples), for streamed sources.
    pub fn rms_spans_overview(&self, start: f64, step: f64, overlap: f64, cols: usize) -> Vec<Option<f32>> {
        self.windows_n(self.len, start, step, overlap, cols)
            .map(|w| {
                w.map(|(a, b)| {
                    let (s, e) = (a.floor() as usize, (b.ceil() as usize).saturating_add(1).min(self.len));
                    (self.sum_pyramid(s, e).squares / e.saturating_sub(s).max(1) as f32).sqrt()
                })
            })
            .collect()
    }

    /// (min, max) of the line through `samples` over each of `cols` spans of sample positions
    /// `[start + c * step, start + (c + 1) * step + overlap]`; fractional positions interpolate
    /// between samples. Neighbouring spans share at least their boundary, so a waveform drawn from
    /// them stays connected at any zoom. `None` for spans outside the data.
    pub fn spans(&self, samples: &[f32], start: f64, step: f64, overlap: f64, cols: usize) -> Vec<Option<(f32, f32)>> {
        let at = |t: f64| {
            let i = t.floor();
            let a = samples.get(i as usize).copied().unwrap_or(0.0);
            let b = samples.get(i as usize + 1).copied().unwrap_or(a);
            Block::point(a + (b - a) * (t - i) as f32)
        };
        self.windows(samples, start, step, overlap, cols)
            .map(|w| w.map(|(a, b)| self.sum(samples, a.ceil() as usize, b.floor() as usize + 1).merge(at(a)).merge(at(b)).range()))
            .collect()
    }

    /// The RMS of `samples` over each span [`Peaks::spans`] gives, including the samples just
    /// outside a span's ends.
    pub fn rms_spans(&self, samples: &[f32], start: f64, step: f64, overlap: f64, cols: usize) -> Vec<Option<f32>> {
        self.windows(samples, start, step, overlap, cols)
            .map(|w| {
                w.map(|(a, b)| {
                    let (s, e) = (a.floor() as usize, (b.ceil() as usize).saturating_add(1).min(self.len).min(samples.len()));
                    (self.sum(samples, s, e).squares / e.saturating_sub(s).max(1) as f32).sqrt()
                })
            })
            .collect()
    }
}

/// A one-pass builder for [`Peaks`] that never holds the whole channel in memory.
///
/// Feed samples with [`PeaksBuilder::push`] / [`PeaksBuilder::push_samples`] (e.g. as a file is
/// streamed from disk), then [`PeaksBuilder::finish`]. The result is bit-for-bit identical to
/// [`Peaks::build`] over the same sequence; the builder keeps only the level-0 blocks (one
/// [`Block`] per [`BLOCK`] samples, far smaller than the samples), plus the in-progress block.
#[derive(Debug, Clone)]
pub struct PeaksBuilder {
    len: usize,
    level0: Vec<Block>,
    partial: Block,
    partial_count: usize,
}

impl Default for PeaksBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PeaksBuilder {
    /// A fresh builder holding no samples.
    pub fn new() -> Self {
        PeaksBuilder { len: 0, level0: Vec::new(), partial: Block::EMPTY, partial_count: 0 }
    }

    /// Append one sample. NaN counts for nothing in the RMS (as in [`Peaks::build`]).
    pub fn push(&mut self, s: f32) {
        self.partial = self.partial.merge(Block::point(s));
        self.partial_count += 1;
        self.len += 1;
        if self.partial_count == BLOCK {
            self.level0.push(self.partial);
            self.partial = Block::EMPTY;
            self.partial_count = 0;
        }
    }

    /// Append a run of samples.
    pub fn push_samples(&mut self, samples: &[f32]) {
        for &s in samples {
            self.push(s);
        }
    }

    /// Number of samples pushed so far.
    pub fn len_samples(&self) -> usize {
        self.len
    }

    /// Build the multi-resolution overview from everything pushed.
    pub fn finish(mut self) -> Peaks {
        if self.len == 0 {
            return Peaks::default();
        }
        if self.partial_count > 0 {
            self.level0.push(self.partial);
        }
        let mut levels = vec![self.level0];
        while let Some(last) = levels.last().filter(|l| l.len() > 1) {
            let next = last.chunks(FANOUT).map(|c| c.iter().fold(Block::EMPTY, |a, &b| a.merge(b))).collect();
            levels.push(next);
        }
        Peaks { len: self.len, levels }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute(samples: &[f32], start: usize, end: usize) -> (f32, f32) {
        let s = &samples[start..end];
        (s.iter().copied().fold(f32::INFINITY, f32::min), s.iter().copied().fold(f32::NEG_INFINITY, f32::max))
    }

    fn signal(n: usize) -> Vec<f32> {
        (0..n).map(|i| ((i as f32 * 0.037).sin() * (i as f32 * 0.0011).cos()) + if i % 997 == 0 { 0.5 } else { 0.0 }).collect()
    }

    #[test]
    fn overview_minmax_covers_the_exact_range() {
        // The overview-only query rounds out to whole blocks, so it must contain the exact (min,
        // max) of any range — never clip it — and must agree exactly on block-aligned ranges.
        let x = signal(50_000);
        let p = Peaks::build(&x);
        for &(a, b) in &[(0, BLOCK), (0, 50_000), (BLOCK, 64 * BLOCK), (100, 200), (12_345, 23_456)] {
            let (elo, ehi) = brute(&x, a, b);
            let (olo, ohi) = p.minmax_overview(a, b);
            assert!(olo <= elo + 1e-6 && ohi >= ehi - 1e-6, "overview {a}..{b} must contain the exact range");
        }
        // Block-aligned ranges are exact.
        assert_eq!(p.minmax_overview(0, 64 * BLOCK), p.minmax(&x, 0, 64 * BLOCK));
        // Empty overview is silent.
        assert_eq!(Peaks::default().minmax_overview(0, 10), (0.0, 0.0));
        // spans_overview produces the requested column count and no sample panics.
        let spans = p.spans_overview(0.0, 123.0, 0.0, 300);
        assert_eq!(spans.len(), 300);
        assert!(p.rms_spans_overview(0.0, 123.0, 0.0, 300).iter().all(|r| r.is_none_or(f32::is_finite)));
    }

    #[test]
    fn every_range_is_exact() {
        let x = signal(100_003);
        let p = Peaks::build(&x);
        for &(a, b) in &[(0, 32), (0, 100_003), (32, 64_000), (3200, 3232), (96, 99_968), (64, 100_003), (5, 6), (31, 33), (1000, 1031)] {
            assert_eq!(p.minmax(&x, a, b), brute(&x, a, b), "range {a}..{b}");
        }
        let mut seed = 12345u64;
        for _ in 0..2000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let a = (seed >> 33) as usize % 100_003;
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let b = a + 1 + (seed >> 33) as usize % (100_003 - a);
            assert_eq!(p.minmax(&x, a, b), brute(&x, a, b), "range {a}..{b}");
        }
    }

    #[test]
    fn degenerate_inputs() {
        let p = Peaks::build(&[]);
        assert_eq!(p.len_samples(), 0);
        assert_eq!(p.minmax(&[], 0, 10), (0.0, 0.0));
        assert_eq!(p.spans(&[], 0.0, 1.0, 0.0, 4), vec![None; 4]);
        let x = [0.5, -0.25];
        let p = Peaks::build(&x);
        assert_eq!(p.minmax(&x, 0, 2), (-0.25, 0.5));
        assert_eq!(p.minmax(&x, 5, 3), (0.0, 0.0));
        assert_eq!(p.minmax(&x, 0, usize::MAX), (-0.25, 0.5));
        assert_eq!(p.minmax(&x[..1], 0, 2), (0.5, 0.5));
        for (start, step, overlap) in [
            (f64::NAN, 1.0, 0.0),
            (0.0, 0.0, 0.0),
            (0.0, -1.0, 0.0),
            (0.0, f64::INFINITY, 0.0),
            (f64::NEG_INFINITY, 1.0, 0.0),
            (0.0, 1.0, -0.5),
            (0.0, 1.0, f64::NAN),
        ] {
            assert_eq!(p.spans(&x, start, step, overlap, 3), vec![None; 3]);
        }
        assert_eq!(p.spans(&x, 0.0, 1.0, 0.0, 0).len(), 0);
        assert_eq!(p.spans(&x, 0.0, 1.0, 0.0, usize::MAX).len(), MAX_COLUMNS);
        let x = [f32::NAN, 1.0];
        let p = Peaks::build(&x);
        assert_eq!(p.minmax(&x, 0, 2), (1.0, 1.0));
        assert_eq!(p.spans(&x, 0.0, 1.0, 0.0, 1), vec![Some((1.0, 1.0))]);
        assert!(p.rms_spans(&x, 0.0, 1.0, 0.0, 1)[0].is_some_and(f32::is_finite));
    }

    #[test]
    fn rms_spans_match_the_samples() {
        let x = signal(20_000);
        let p = Peaks::build(&x);
        let rms = |s: usize, e: usize| (x[s..e].iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / (e - s) as f64).sqrt() as f32;
        for (step, overlap) in [(1.5, 0.0), (37.0, 37.0), (1000.0, 0.0), (4096.0, 512.0)] {
            let cols = ((19_999.0 - overlap) / step) as usize;
            for (c, got) in p.rms_spans(&x, 0.0, step, overlap, cols).into_iter().enumerate() {
                let (a, b) = (c as f64 * step, ((c + 1) as f64 * step + overlap).min(19_999.0));
                let (got, want) = (got.unwrap(), rms(a.floor() as usize, (b.ceil() as usize + 1).min(20_000)));
                assert!((got - want).abs() <= want * 1e-4 + 1e-6, "span {c} at step {step}: {got} vs {want}");
            }
        }
        let sine: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin()).collect();
        let p = Peaks::build(&sine);
        let r = p.rms_spans(&sine, 0.0, 47_000.0, 0.0, 1)[0].unwrap();
        assert!((r - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3, "{r}");
        assert_eq!(p.rms_spans(&sine, f64::NAN, 1.0, 0.0, 2), vec![None, None]);
        assert_eq!(p.rms_spans(&sine, 50_000.0, 1.0, 0.0, 1), vec![None]);
    }

    #[test]
    fn spans_cover_their_samples_and_touch_their_neighbours() {
        let x = signal(10_000);
        let p = Peaks::build(&x);
        for step in [0.3, 1.0, 7.5, 100.0, 333.3] {
            let cols = (9_999.0 / step) as usize;
            let spans = p.spans(&x, 0.0, step, 0.0, cols);
            for (c, s) in spans.iter().enumerate() {
                let (lo, hi) = s.unwrap_or_else(|| panic!("span {c} at step {step}"));
                let (a, b) = ((c as f64 * step).ceil() as usize, ((c + 1) as f64 * step).floor() as usize);
                if a <= b {
                    let truth = brute(&x, a, b + 1);
                    assert!(lo <= truth.0 && hi >= truth.1, "span {c} at step {step}");
                }
                if let Some(Some((nlo, nhi))) = spans.get(c + 1) {
                    assert!(*nlo <= hi && *nhi >= lo, "spans {c} and {} at step {step} don't touch", c + 1);
                }
            }
            let all = spans.iter().flatten().fold((f32::INFINITY, f32::NEG_INFINITY), |a, s| (a.0.min(s.0), a.1.max(s.1)));
            assert_eq!(all, brute(&x, 0, ((cols as f64 * step).floor() as usize + 1).min(10_000)), "step {step}");
        }
    }

    #[test]
    fn streaming_builder_matches_build() {
        for n in [0usize, 1, 7, 31, 32, 33, 127, 128, 129, 1000, 4096, 100_003] {
            let x = signal(n);
            let direct = Peaks::build(&x);
            // One big push.
            let mut b = PeaksBuilder::new();
            b.push_samples(&x);
            let streamed = b.finish();
            assert_eq!(direct, streamed, "one-shot push, n={n}");
            // Many small odd-sized pushes (never aligned to BLOCK).
            let mut b = PeaksBuilder::new();
            for chunk in x.chunks(7) {
                b.push_samples(chunk);
            }
            assert_eq!(b.len_samples(), n);
            assert_eq!(direct, b.finish(), "chunked push, n={n}");
        }
        // NaN handling matches build().
        let x = [f32::NAN, 1.0, -0.5, f32::NAN];
        let mut b = PeaksBuilder::new();
        b.push_samples(&x);
        assert_eq!(Peaks::build(&x), b.finish());
    }

    #[test]
    fn spans_follow_the_line_between_samples() {
        let x = [0.0, 1.0, -1.0, 0.5];
        let p = Peaks::build(&x);
        assert_eq!(p.spans(&x, 0.0, 0.25, 0.0, 4), vec![Some((0.0, 0.25)), Some((0.25, 0.5)), Some((0.5, 0.75)), Some((0.75, 1.0))]);
        assert_eq!(p.spans(&x, 0.5, 1.0, 0.0, 1), vec![Some((0.0, 1.0))]);
        let spans = vec![None, Some((0.0, 0.0)), Some((0.0, 1.0)), Some((-1.0, 1.0)), Some((-1.0, 0.5)), Some((0.5, 0.5)), None];
        assert_eq!(p.spans(&x, -2.0, 1.0, 0.0, 7), spans);
        assert_eq!(p.spans(&x, 0.0, 1.0, 1.0, 3), vec![Some((-1.0, 1.0)), Some((-1.0, 1.0)), Some((-1.0, 0.5))]);
        assert_eq!(p.spans(&x, 0.0, 1.0, 0.5, 3), vec![Some((0.0, 1.0)), Some((-1.0, 1.0)), Some((-1.0, 0.5))]);
    }
}
