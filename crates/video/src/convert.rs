//! Pixel conversion: Y'CbCr → RGBA (BT.601 / BT.709 / BT.2020 non-constant-luminance, limited or
//! full range, 8 to 16 bits per sample) and RGBA downscaling.
//!
//! Output is always 8-bit sRGB-style RGBA. Wide-gamut and high-dynamic-range pictures (BT.2020
//! primaries, PQ or HLG transfer) are converted with the picture's own matrix only: there is no
//! gamut mapping and no tone mapping, so HDR material looks flat and desaturated on an SDR display.

/// Y'CbCr → R'G'B' matrix coefficients in 1/65536 units: (Cr→R, Cb→G, Cr→G, Cb→B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Matrix {
    cr_r: i32,
    cb_g: i32,
    cr_g: i32,
    cb_b: i32,
    full_range: bool,
}

impl Matrix {
    /// BT.601 (SD) or BT.709 (HD) coefficients.
    pub fn new(bt709: bool, full_range: bool) -> Matrix {
        // Kr/Kb: 601 = 0.299/0.114, 709 = 0.2126/0.0722.
        let (kr, kb) = if bt709 { (0.2126f64, 0.0722f64) } else { (0.299, 0.114) };
        Matrix::from_k(kr, kb, full_range)
    }

    /// BT.2020 non-constant-luminance coefficients (matrix code 9).
    pub fn bt2020(full_range: bool) -> Matrix {
        Matrix::from_k(0.2627, 0.0593, full_range)
    }

    fn from_k(kr: f64, kb: f64, full_range: bool) -> Matrix {
        let kg = 1.0 - kr - kb;
        let s = 65536.0;
        Matrix {
            cr_r: (2.0 * (1.0 - kr) * s).round() as i32,
            cb_b: (2.0 * (1.0 - kb) * s).round() as i32,
            cb_g: (2.0 * (1.0 - kb) * kb / kg * s).round() as i32,
            cr_g: (2.0 * (1.0 - kr) * kr / kg * s).round() as i32,
            full_range,
        }
    }

    /// Choose by ITU-T H.273 matrix code (1 = 709, 5/6 = 601, 9 = 2020 non-constant luminance),
    /// else by picture height (600 rows and up are taken as 709). Code 10 (2020 constant
    /// luminance) is not implemented and also falls back to the height rule.
    pub fn for_code(matrix: u8, height: u32, full_range: bool) -> Matrix {
        match matrix {
            9 => Matrix::bt2020(full_range),
            1 => Matrix::new(true, full_range),
            5 | 6 => Matrix::new(false, full_range),
            _ => Matrix::new(height >= 600, full_range),
        }
    }

    /// Convert one 8-bit sample triple.
    #[inline]
    pub fn rgb(&self, y: u8, cb: u8, cr: u8) -> [u8; 3] {
        let (y, cb, cr) = (i32::from(y), i32::from(cb) - 128, i32::from(cr) - 128);
        let (y, cb, cr) = if self.full_range {
            (y << 16, cb, cr)
        } else {
            // Expand 16..235 / 16..240 to full range.
            ((y - 16) * 76309, (cb * 74_606) >> 16, (cr * 74_606) >> 16)
        };
        let y = y + (1 << 15); // round to nearest
        let r = (y + self.cr_r * cr) >> 16;
        let g = (y - self.cb_g * cb - self.cr_g * cr) >> 16;
        let b = (y + self.cb_b * cb) >> 16;
        [r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8]
    }

    /// Convert one sample triple of `bit_depth` bits (8 to 16; other depths are clamped into that
    /// range) to 8-bit R'G'B'. Limited range puts black at `16 << (depth - 8)` and white at
    /// `235 << (depth - 8)`; full range spans `0..=2^depth - 1`. Samples outside the range clamp.
    #[inline]
    pub fn rgb16(&self, y: u16, cb: u16, cr: u16, bit_depth: u8) -> [u8; 3] {
        self.rgb16_with(&Depth::new(bit_depth), y, cb, cr)
    }

    #[inline]
    fn rgb16_with(&self, d: &Depth, y: u16, cb: u16, cr: u16) -> [u8; 3] {
        let (y, cb, cr) = (i64::from(y), i64::from(cb) - d.half, i64::from(cr) - d.half);
        // Everything below is in 1/65536 of an 8-bit code value.
        let (yq, cbq, crq) = if self.full_range {
            ((y * d.k) >> 16, (cb * d.k) >> 16, (cr * d.k) >> 16)
        } else {
            (((y - d.black) * 76309) >> d.sh, (cb * 74_606) >> d.sh, (cr * 74_606) >> d.sh)
        };
        let yq = yq + (1 << 15); // round to nearest
        let r = (yq + ((i64::from(self.cr_r) * crq) >> 16)) >> 16;
        let g = (yq - ((i64::from(self.cb_g) * cbq) >> 16) - ((i64::from(self.cr_g) * crq) >> 16)) >> 16;
        let b = (yq + ((i64::from(self.cb_b) * cbq) >> 16)) >> 16;
        [r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8]
    }
}

/// Constants for one sample bit depth (8 to 16), computed once per picture.
#[derive(Debug, Clone, Copy)]
struct Depth {
    /// `bit_depth - 8`.
    sh: u32,
    /// Neutral chroma, `128 << sh`.
    half: i64,
    /// Limited-range black, `16 << sh`.
    black: i64,
    /// Full range: `255 * 2^32 / (2^bit_depth - 1)`, so `v * k >> 16` is `v` in 1/65536 of an 8-bit code.
    k: i64,
}

impl Depth {
    fn new(bit_depth: u8) -> Depth {
        let bits = u32::from(bit_depth.clamp(8, 16));
        let sh = bits - 8;
        let max = (1i64 << bits) - 1;
        Depth { sh, half: 128 << sh, black: 16 << sh, k: (255i64 << 32) / max }
    }
}

/// A zeroed RGBA buffer for `width` × `height`, or `None` when the picture is empty or larger than
/// [`crate::movie::MAX_PIXELS`] (so a hostile size can never request a huge allocation).
fn rgba_buffer(width: usize, height: usize) -> Option<Vec<u8>> {
    let px = (width as u64).checked_mul(height as u64)?;
    if px == 0 || px > crate::movie::MAX_PIXELS {
        return None;
    }
    Some(vec![0u8; usize::try_from(px).ok()?.checked_mul(4)?])
}

/// Planar 8-bit Y'CbCr with chroma subsampled by `cx` horizontally and `cy` vertically
/// (1 or 2 each) → packed RGBA. Missing samples (short planes) read as black / neutral.
pub fn planar_to_rgba(
    width: usize,
    height: usize,
    y: &[u8],
    y_stride: usize,
    u: &[u8],
    v: &[u8],
    uv_stride: usize,
    cx: usize,
    cy: usize,
    m: &Matrix,
) -> Vec<u8> {
    let (cx, cy) = (cx.max(1), cy.max(1));
    let Some(mut out) = rgba_buffer(width, height) else { return Vec::new() };
    for (row, line) in out.chunks_exact_mut(width * 4).enumerate().take(height) {
        let yl = y.get(row.saturating_mul(y_stride)..).unwrap_or(&[]);
        let crow = (row / cy).saturating_mul(uv_stride);
        let ul = u.get(crow..).unwrap_or(&[]);
        let vl = v.get(crow..).unwrap_or(&[]);
        for (col, px) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let yy = yl.get(col).copied().unwrap_or(16);
            let cb = ul.get(col / cx).copied().unwrap_or(128);
            let cr = vl.get(col / cx).copied().unwrap_or(128);
            let [r, g, b] = m.rgb(yy, cb, cr);
            px.copy_from_slice(&[r, g, b, 255]);
        }
    }
    out
}

/// Planar Y'CbCr with 9 to 16-bit samples (held in `u16`, value range `0..2^bit_depth`) and chroma
/// subsampled by `cx` horizontally and `cy` vertically (1 or 2 each; empty `u` and `v` mean a
/// monochrome picture, which reads as neutral grey) → packed 8-bit RGBA. Missing samples (short
/// planes) read as black / neutral. `bit_depth` 8 works too. Strides are in samples.
pub fn planar16_to_rgba(
    width: usize,
    height: usize,
    y: &[u16],
    y_stride: usize,
    u: &[u16],
    v: &[u16],
    uv_stride: usize,
    cx: usize,
    cy: usize,
    bit_depth: u8,
    m: &Matrix,
) -> Vec<u8> {
    let (cx, cy) = (cx.max(1), cy.max(1));
    let d = Depth::new(bit_depth);
    // A missing luma sample is black in the matrix's own range.
    let black = if m.full_range { 0 } else { u16::try_from(d.black).unwrap_or(0) };
    let neutral = u16::try_from(d.half).unwrap_or(0);
    let Some(mut out) = rgba_buffer(width, height) else { return Vec::new() };
    for (row, line) in out.chunks_exact_mut(width * 4).enumerate().take(height) {
        let yl = y.get(row.saturating_mul(y_stride)..).unwrap_or(&[]);
        let crow = (row / cy).saturating_mul(uv_stride);
        let ul = u.get(crow..).unwrap_or(&[]);
        let vl = v.get(crow..).unwrap_or(&[]);
        for (col, px) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let yy = yl.get(col).copied().unwrap_or(black);
            let cb = ul.get(col / cx).copied().unwrap_or(neutral);
            let cr = vl.get(col / cx).copied().unwrap_or(neutral);
            let [r, g, b] = m.rgb16_with(&d, yy, cb, cr);
            px.copy_from_slice(&[r, g, b, 255]);
        }
    }
    out
}

/// Planar R'G'B' (three full-size planes with a common `stride`, in samples) with 8 to 16-bit
/// samples → packed 8-bit RGBA. `full_range` false is video range (black at `16 << (depth - 8)`,
/// white at `235 << (depth - 8)`, as in DNxHR RGB); true spans `0..=2^depth - 1`. Missing samples
/// read as black.
pub fn rgb16_to_rgba(width: usize, height: usize, r: &[u16], g: &[u16], b: &[u16], stride: usize, bit_depth: u8, full_range: bool) -> Vec<u8> {
    let d = Depth::new(bit_depth);
    let black = u16::try_from(d.black).unwrap_or(0);
    let level = |v: u16| -> u8 {
        let q = if full_range { (i64::from(v) * d.k) >> 16 } else { ((i64::from(v) - d.black) * 76309) >> d.sh };
        ((q + (1 << 15)) >> 16).clamp(0, 255) as u8
    };
    let Some(mut out) = rgba_buffer(width, height) else { return Vec::new() };
    for (row, line) in out.chunks_exact_mut(width * 4).enumerate().take(height) {
        let at = row.saturating_mul(stride);
        let (rl, gl, bl) = (r.get(at..).unwrap_or(&[]), g.get(at..).unwrap_or(&[]), b.get(at..).unwrap_or(&[]));
        for (col, px) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let at = |l: &[u16]| level(l.get(col).copied().unwrap_or(if full_range { 0 } else { black }));
            px.copy_from_slice(&[at(rl), at(gl), at(bl), 255]);
        }
    }
    out
}

/// Sample storage of a [`Yuv`] picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planes {
    /// 8-bit samples.
    U8 { y: Vec<u8>, u: Vec<u8>, v: Vec<u8> },
    /// 9 to 16-bit samples, value range `0..2^bit_depth`.
    U16 { y: Vec<u16>, u: Vec<u16>, v: Vec<u16> },
}

/// A decoded planar Y'CbCr picture, as long-GOP decoders hand it to [`crate::Movie`], which
/// converts it to RGBA with [`Yuv::to_rgba`] only when a frame is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Yuv {
    /// Cropped luma size in pixels.
    pub width: u32,
    pub height: u32,
    pub planes: Planes,
    /// Row strides in samples.
    pub y_stride: usize,
    pub uv_stride: usize,
    /// Chroma subsampling: 2 = halved, 1 = full resolution (4:2:0 is 2 and 2, 4:2:2 is 2 and 1,
    /// 4:4:4 is 1 and 1). Empty chroma planes mean a monochrome picture.
    pub cx: usize,
    pub cy: usize,
    /// Bits per sample (8 for [`Planes::U8`]).
    pub bit_depth: u8,
    /// ITU-T H.273 matrix coefficients code (0 or anything unknown picks 601 or 709 by height).
    pub matrix: u8,
    pub full_range: bool,
}

impl Yuv {
    /// Convert to packed 8-bit RGBA (`width * height * 4` bytes; empty when the size is zero or
    /// larger than this crate decodes).
    pub fn to_rgba(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let m = Matrix::for_code(self.matrix, self.height, self.full_range);
        match &self.planes {
            Planes::U8 { y, u, v } => planar_to_rgba(w, h, y, self.y_stride, u, v, self.uv_stride, self.cx, self.cy, &m),
            Planes::U16 { y, u, v } => planar16_to_rgba(w, h, y, self.y_stride, u, v, self.uv_stride, self.cx, self.cy, self.bit_depth, &m),
        }
    }
}

/// Area-average downscale (or nearest upscale) of packed RGBA.
pub fn scale_rgba(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let (dw, dh) = (dw.max(1), dh.max(1));
    let mut out = vec![0u8; dw * dh * 4];
    if sw == 0 || sh == 0 || src.len() < sw * sh * 4 {
        return out;
    }
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1).min(sh);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1).min(sw);
            // Sample at most a 4x4 grid inside the source box (keeps big reductions cheap).
            let (sx, sy) = ((x1 - x0).div_ceil(4).max(1), (y1 - y0).div_ceil(4).max(1));
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            let mut y = y0;
            while y < y1 {
                let mut x = x0;
                while x < x1 {
                    if let Some(p) = src.get((y * sw + x) * 4..(y * sw + x) * 4 + 4) {
                        for (a, v) in acc.iter_mut().zip(p) {
                            *a += u32::from(*v);
                        }
                        n += 1;
                    }
                    x += sx;
                }
                y += sy;
            }
            if let Some(o) = out.get_mut((dy * dw + dx) * 4..(dy * dw + dx) * 4 + 4) {
                for (o, a) in o.iter_mut().zip(acc) {
                    *o = (a / n.max(1)) as u8;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_range_black_white_and_grey() {
        for bt709 in [false, true] {
            let m = Matrix::new(bt709, false);
            assert_eq!(m.rgb(16, 128, 128), [0, 0, 0]);
            assert_eq!(m.rgb(235, 128, 128), [255, 255, 255]);
            let g = m.rgb(126, 128, 128);
            assert!(g[0] == g[1] && g[1] == g[2]);
        }
        let f = Matrix::new(true, true);
        assert_eq!(f.rgb(0, 128, 128), [0, 0, 0]);
        assert_eq!(f.rgb(255, 128, 128), [255, 255, 255]);
    }

    #[test]
    fn red_is_red() {
        // BT.601 limited-range red: Y 81, Cb 90, Cr 240.
        let [r, g, b] = Matrix::new(false, false).rgb(81, 90, 240);
        assert!(r > 250 && g < 5 && b < 5, "{r} {g} {b}");
    }

    #[test]
    fn scaling_averages_and_survives_bad_input() {
        let src = [255u8, 255, 255, 255, 0, 0, 0, 255];
        let out = scale_rgba(&src, 2, 1, 1, 1);
        assert_eq!(out, vec![127, 127, 127, 255]);
        assert_eq!(scale_rgba(&[], 10, 10, 2, 2), vec![0; 16]);
        assert_eq!(scale_rgba(&src, 2, 1, 4, 2).len(), 32);
    }

    /// Forward transform (f64): R'G'B' 0..=1 → Y'CbCr codes at `bits`, with the given weights.
    fn encode(kr: f64, kb: f64, rgb: [f64; 3], bits: u32, full: bool) -> [u16; 3] {
        let kg = 1.0 - kr - kb;
        let y = kr * rgb[0] + kg * rgb[1] + kb * rgb[2];
        let cb = (rgb[2] - y) / (2.0 * (1.0 - kb));
        let cr = (rgb[0] - y) / (2.0 * (1.0 - kr));
        let s = f64::from(1u32 << (bits - 8));
        let max = f64::from((1u32 << bits) - 1);
        let q = |v: f64| v.round().clamp(0.0, max) as u16;
        if full {
            [q(y * max), q(cb * max + 0.5 * (max + 1.0)), q(cr * max + 0.5 * (max + 1.0))]
        } else {
            [q((16.0 + 219.0 * y) * s), q((128.0 + 224.0 * cb) * s), q((128.0 + 224.0 * cr) * s)]
        }
    }

    const COLORS: [[f64; 3]; 8] =
        [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.9, 0.6, 0.1], [0.2, 0.4, 0.8], [0.5, 0.5, 0.5]];

    #[test]
    fn every_matrix_and_depth_round_trips_colours() {
        for (code, kr, kb) in [(5u8, 0.299, 0.114), (1, 0.2126, 0.0722), (9, 0.2627, 0.0593)] {
            for full in [false, true] {
                let m = Matrix::for_code(code, 1080, full);
                for bits in [8u32, 10, 12, 14, 16] {
                    for rgb in COLORS {
                        let [y, cb, cr] = encode(kr, kb, rgb, bits, full);
                        let got = m.rgb16(y, cb, cr, bits as u8);
                        // 8-bit codes carry up to half a code of quantisation error per component.
                        let tol = if bits == 8 { 2 } else { 1 };
                        for c in 0..3 {
                            let want = (rgb[c] * 255.0).round() as i32;
                            assert!((i32::from(got[c]) - want).abs() <= tol, "code {code} full {full} {bits}-bit {rgb:?}: {got:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bt2020_is_selected_by_code_9_and_differs_from_709() {
        assert_eq!(Matrix::for_code(9, 2160, false), Matrix::bt2020(false));
        assert_eq!(Matrix::for_code(9, 480, true), Matrix::bt2020(true));
        assert_ne!(Matrix::bt2020(false), Matrix::new(true, false));
        // The same codes decode to visibly different greens under 709 and 2020.
        let [y, cb, cr] = encode(0.2627, 0.0593, [0.0, 1.0, 0.0], 10, false);
        let g2020 = Matrix::bt2020(false).rgb16(y, cb, cr, 10);
        let g709 = Matrix::new(true, false).rgb16(y, cb, cr, 10);
        assert!(g2020[0] <= 1 && g2020[1] >= 254 && g2020[2] <= 1, "{g2020:?}");
        assert!((i32::from(g2020[1]) - i32::from(g709[1])).abs() >= 5, "{g2020:?} vs {g709:?}");
        // Other codes are unchanged: 709, 601 and the height rule.
        assert_eq!(Matrix::for_code(1, 100, false), Matrix::new(true, false));
        assert_eq!(Matrix::for_code(6, 2160, false), Matrix::new(false, false));
        assert_eq!(Matrix::for_code(2, 480, false), Matrix::new(false, false));
        assert_eq!(Matrix::for_code(2, 1080, false), Matrix::new(true, false));
        assert_eq!(Matrix::for_code(10, 1080, false), Matrix::new(true, false));
    }

    #[test]
    fn wide_samples_agree_with_the_8_bit_path_at_8_bits() {
        for bt709 in [false, true] {
            for full in [false, true] {
                let m = Matrix::new(bt709, full);
                for y in (0..=255u16).step_by(5) {
                    for cb in (0..=255u16).step_by(17) {
                        for cr in (0..=255u16).step_by(17) {
                            let a = m.rgb(y as u8, cb as u8, cr as u8);
                            let b = m.rgb16(y, cb, cr, 8);
                            // The 8-bit path floors its chroma expansion in limited range.
                            let tol = if full { 1 } else { 2 };
                            for c in 0..3 {
                                assert!((i32::from(a[c]) - i32::from(b[c])).abs() <= tol, "{y} {cb} {cr}: {a:?} {b:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn limited_range_10_bit_black_white_and_clamping() {
        let m = Matrix::new(true, false);
        assert_eq!(m.rgb16(64, 512, 512, 10), [0, 0, 0]);
        assert_eq!(m.rgb16(940, 512, 512, 10), [255, 255, 255]);
        assert_eq!(m.rgb16(0, 512, 512, 10), [0, 0, 0], "below black clamps");
        assert_eq!(m.rgb16(1023, 512, 512, 10), [255, 255, 255], "above white clamps");
        let f = Matrix::new(true, true);
        assert_eq!(f.rgb16(0, 512, 512, 10), [0, 0, 0]);
        assert_eq!(f.rgb16(1023, 512, 512, 10), [255, 255, 255]);
        // Out-of-range bit depths and sample values never panic.
        for bits in [0u8, 7, 8, 16, 17, 255] {
            let _ = m.rgb16(u16::MAX, 0, u16::MAX, bits);
            let _ = f.rgb16(0, u16::MAX, 0, bits);
        }
    }

    #[test]
    fn planar16_handles_every_subsampling_and_mono() {
        let m = Matrix::new(true, false);
        // 4x2 picture, 10-bit: left half white, right half black.
        let y: Vec<u16> = [940, 940, 64, 64, 940, 940, 64, 64].to_vec();
        let grey = |n: usize| vec![512u16; n];
        for (cx, cy) in [(2usize, 2usize), (2, 1), (1, 1)] {
            let (cw, ch) = (4usize.div_ceil(cx), 2usize.div_ceil(cy));
            let out = planar16_to_rgba(4, 2, &y, 4, &grey(cw * ch), &grey(cw * ch), cw, cx, cy, 10, &m);
            assert_eq!(out.len(), 32);
            for row in 0..2 {
                for col in 0..4 {
                    let want = if col < 2 { 255 } else { 0 };
                    assert_eq!(&out[(row * 4 + col) * 4..][..4], &[want, want, want, 255], "{cx}x{cy} at {col},{row}");
                }
            }
        }
        // Monochrome: no chroma planes → neutral grey.
        let out = planar16_to_rgba(4, 2, &y, 4, &[], &[], 0, 2, 2, 10, &m);
        assert_eq!(&out[..4], &[255, 255, 255, 255]);
        assert_eq!(&out[8..12], &[0, 0, 0, 255]);
        // Padded rows: stride larger than the width.
        let padded: Vec<u16> = [940, 64, 0, 0, 940, 64, 0, 0].to_vec();
        let out = planar16_to_rgba(2, 2, &padded, 4, &grey(4), &grey(4), 2, 1, 1, 10, &m);
        assert_eq!((&out[..4], &out[4..8], &out[8..12]), (&[255u8, 255, 255, 255][..], &[0u8, 0, 0, 255][..], &[255u8, 255, 255, 255][..]));
        // 16-bit samples span the whole range.
        let out = planar16_to_rgba(2, 1, &[16 << 8, 235 << 8], 2, &[], &[], 0, 2, 2, 16, &m);
        assert_eq!((&out[..3], &out[4..7]), (&[0u8, 0, 0][..], &[255u8, 255, 255][..]));
    }

    #[test]
    fn planar16_survives_bad_input() {
        let m = Matrix::new(false, true);
        // Short planes read as black with neutral chroma.
        let out = planar16_to_rgba(4, 4, &[1023; 3], 4, &[], &[], 2, 2, 2, 10, &m);
        assert_eq!(out.len(), 64);
        assert_eq!(&out[..4], &[255, 255, 255, 255]);
        assert_eq!(&out[12..16], &[0, 0, 0, 255]);
        // Absurd strides and subsampling factors are fine.
        let _ = planar16_to_rgba(3, 3, &[0; 9], usize::MAX, &[0; 9], &[0; 9], usize::MAX, 0, 0, 12, &m);
        // Empty and oversized pictures return nothing instead of allocating.
        assert!(planar16_to_rgba(0, 10, &[], 0, &[], &[], 0, 2, 2, 10, &m).is_empty());
        assert!(planar16_to_rgba(100_000, 100_000, &[], 0, &[], &[], 0, 2, 2, 10, &m).is_empty());
        assert!(planar16_to_rgba(usize::MAX, usize::MAX, &[], 0, &[], &[], 0, 2, 2, 10, &m).is_empty());
        assert!(planar_to_rgba(100_000, 100_000, &[], 0, &[], &[], 0, 2, 2, &m).is_empty());
        assert!(rgb16_to_rgba(100_000, 100_000, &[], &[], &[], 0, 10, false).is_empty());
    }

    #[test]
    fn rgb16_video_and_full_range() {
        // 10-bit video range: 64 is black, 940 is white, mid-grey 502 → 128.
        let r = [64u16, 940, 502, 64];
        let g = [64u16, 940, 502, 940];
        let b = [64u16, 940, 502, 64];
        let out = rgb16_to_rgba(4, 1, &r, &g, &b, 4, 10, false);
        assert_eq!(&out[..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..8], &[255, 255, 255, 255]);
        assert!((i32::from(out[8]) - 128).abs() <= 1 && out[8] == out[9] && out[9] == out[10], "{:?}", &out[8..12]);
        assert_eq!(&out[12..16], &[0, 255, 0, 255], "planes are R, G, B");
        // Full range spans 0..=1023.
        let out = rgb16_to_rgba(2, 1, &[0, 1023], &[0, 1023], &[0, 512], 2, 10, true);
        assert_eq!(&out[..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..8], &[255, 255, 128, 255]);
        // 8-bit video range and short planes.
        let out = rgb16_to_rgba(2, 1, &[16, 235], &[16, 235], &[16], 2, 8, false);
        assert_eq!(&out[..8], &[0, 0, 0, 255, 255, 255, 0, 255]);
        // Stride padding.
        let out = rgb16_to_rgba(1, 2, &[940, 0, 64], &[940, 0, 64], &[940, 0, 64], 2, 10, false);
        assert_eq!((&out[..3], &out[4..7]), (&[255u8, 255, 255][..], &[0u8, 0, 0][..]));
    }

    #[test]
    fn yuv_converts_like_the_free_functions() {
        let (y8, c8) = (vec![180u8; 16], vec![128u8; 4]);
        let narrow = Yuv {
            width: 4,
            height: 4,
            planes: Planes::U8 { y: y8.clone(), u: c8.clone(), v: c8.clone() },
            y_stride: 4,
            uv_stride: 2,
            cx: 2,
            cy: 2,
            bit_depth: 8,
            matrix: 1,
            full_range: false,
        };
        let m = Matrix::new(true, false);
        assert_eq!(narrow.to_rgba(), planar_to_rgba(4, 4, &y8, 4, &c8, &c8, 2, 2, 2, &m));
        let wide = Yuv { planes: Planes::U16 { y: vec![720; 16], u: vec![512; 4], v: vec![512; 4] }, bit_depth: 10, matrix: 9, ..narrow.clone() };
        let px = wide.to_rgba();
        assert_eq!(px.len(), 64);
        assert_eq!(&px[..4], &[m.rgb16(720, 512, 512, 10)[0], m.rgb16(720, 512, 512, 10)[1], m.rgb16(720, 512, 512, 10)[2], 255]);
        // A picture that is too large, or empty, converts to nothing.
        assert!(Yuv { width: 100_000, height: 100_000, ..wide.clone() }.to_rgba().is_empty());
        assert!(Yuv { width: 0, ..wide }.to_rgba().is_empty());
    }

    #[test]
    fn planar_conversion_tolerates_short_planes() {
        let m = Matrix::new(true, false);
        let out = planar_to_rgba(4, 4, &[235; 3], 4, &[], &[], 2, 2, 2, &m);
        assert_eq!(out.len(), 64);
        assert_eq!(&out[..4], &[255, 255, 255, 255]);
    }
}
