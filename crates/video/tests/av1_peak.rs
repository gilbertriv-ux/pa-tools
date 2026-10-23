//! Does the AV1 decoder stay inside its memory estimate? A key frame and seven intra-only frames,
//! each refreshing its own reference slot, make eight distinct references; the process's peak
//! resident memory while they decode must stay under `memory_needed` (the figure the decoder checks
//! against its budget), on one thread.
//!
//! Linux only: the peak comes from `/proc/self/status` (`VmHWM`) after resetting it through
//! `/proc/self/clear_refs`, because this workspace forbids the `unsafe` a counting allocator needs.
//! Resident memory counts touched pages only, so it reads a little below what was allocated; the
//! figures in `docs/video-codecs.md` ("Limits") come from a counting allocator in a scratch harness.
//! The stream builders are those of `av1_dims.rs` (from FilmCraft's `hostile_dims` test, MIT OR
//! Apache-2.0), extended with the full sequence header, intra-only frames and film grain.
#![cfg(all(feature = "av1-decode", target_os = "linux"))]

use soundcraft_video::av1::{DEFAULT_MEMORY_BUDGET, Decoder, SequenceHeader, memory_needed, picture_fits_budget};
use std::sync::Mutex;

/// The peak is the whole process's, so measurements must not overlap.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    n: usize,
}
impl Bits {
    fn put(&mut self, value: u32, bits: usize) {
        for i in (0..bits).rev() {
            if self.n.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (value >> i) & 1 == 1 {
                let b = self.bytes.last_mut().unwrap();
                *b |= 0x80 >> (self.n % 8);
            }
            self.n += 1;
        }
    }
}
fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![kind << 3 | 0b10];
    let mut n = payload.len();
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        v.push(if n > 0 { b | 0x80 } else { b });
        if n == 0 {
            break;
        }
    }
    v.extend_from_slice(payload);
    v
}
fn tile_log2(blk: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blk << k) < target {
        k += 1;
    }
    k
}

#[derive(Clone, Copy, Debug)]
struct Fmt {
    profile: u32,
    depth: u32,
    mono: bool,
    grain: bool,
}
impl Fmt {
    fn planes(&self) -> u32 {
        if self.mono { 1 } else { 3 }
    }
    /// color_config() bits (no colour description, separate_uv_delta_q 0).
    fn color(&self, s: &mut Bits) {
        let high = u32::from(self.depth > 8);
        s.put(high, 1);
        if self.profile == 2 && high == 1 {
            s.put(u32::from(self.depth == 12), 1);
        }
        if self.profile != 1 {
            s.put(u32::from(self.mono), 1);
        }
        s.put(0, 1); // color_description_present_flag
        if self.mono {
            s.put(0, 1); // color_range, then return (no separate_uv_delta_q)
            return;
        }
        s.put(0, 1); // color_range
        match self.profile {
            0 => s.put(0, 2), // chroma_sample_position
            1 => {}
            _ => {
                if self.depth == 12 {
                    s.put(0, 1); // subsampling_x = 0 -> 4:4:4
                }
            }
        }
        s.put(0, 1); // separate_uv_delta_q
    }
    /// Lossless quantisation .. reduced_tx_set, then film grain params if present.
    fn tail(&self, f: &mut Bits) {
        f.put(0, 8); // base_q_idx
        f.put(0, 1); // DeltaQYDc
        if self.planes() > 1 {
            f.put(0, 2); // DeltaQUDc, DeltaQUAc
        }
        f.put(0, 1); // using_qmatrix
        f.put(0, 1); // segmentation_enabled
        f.put(0, 1); // reduced_tx_set
        if self.grain {
            f.put(1, 1); // apply_grain
            f.put(0x1234, 16); // grain_seed
            f.put(2, 4); // num_y_points
            f.put(0, 8);
            f.put(64, 8);
            f.put(255, 8);
            f.put(64, 8);
            if !self.mono {
                f.put(1, 1); // chroma_scaling_from_luma
            }
            f.put(0, 2); // grain_scaling_minus_8
            f.put(0, 2); // ar_coeff_lag 0
            if !self.mono {
                f.put(128, 8); // ar_coeffs_cb_plus_128[0]
                f.put(128, 8); // ar_coeffs_cr_plus_128[0]
            }
            f.put(0, 2); // ar_coeff_shift_minus_6
            f.put(0, 2); // grain_scale_shift
            f.put(0, 1); // overlap_flag
            f.put(0, 1); // clip_to_restricted_range
        }
    }
}

/// Tile layout bits and the tile group bytes for uniform spacing (as av1_dims.rs).
fn tiles(w: u32, h: u32) -> (u32, u32, u32) {
    let (sb_cols, sb_rows) = (w.div_ceil(64), h.div_ceil(64));
    let cols_log2 = tile_log2(64, sb_cols);
    let min_log2_tiles = cols_log2.max(tile_log2(2304, sb_rows * sb_cols));
    let rows_log2 = min_log2_tiles.saturating_sub(cols_log2);
    let n = (sb_cols.div_ceil(sb_cols.div_ceil(1 << cols_log2))) * (sb_rows.div_ceil(sb_rows.div_ceil(1 << rows_log2)));
    (cols_log2, rows_log2, n)
}

fn tile_group(frame: &mut Vec<u8>, n: u32, bytes_per_tile: usize) {
    frame.push(0); // tile_start_and_end_present_flag (+ alignment)
    for _ in 1..n {
        let sz = (bytes_per_tile - 1) as u32;
        frame.extend(sz.to_le_bytes()); // tile_size_minus_1 in 4 bytes (tile_size_bytes = 4)
        frame.extend(std::iter::repeat_n(0u8, bytes_per_tile));
    }
    frame.extend(std::iter::repeat_n(0u8, bytes_per_tile));
}

/// Full (non-reduced) sequence header without order hints or screen content tools.
fn full_sequence(w: u32, h: u32, fm: Fmt) -> Vec<u8> {
    let mut s = Bits::default();
    s.put(fm.profile, 3);
    s.put(0, 1); // still_picture
    s.put(0, 1); // reduced_still_picture_header
    s.put(0, 1); // timing_info_present_flag
    s.put(0, 1); // initial_display_delay_present_flag
    s.put(0, 5); // operating_points_cnt_minus_1
    s.put(0, 12); // operating_point_idc[0]
    s.put(0, 5); // seq_level_idx[0]
    s.put(15, 4);
    s.put(15, 4);
    s.put(w - 1, 16);
    s.put(h - 1, 16);
    s.put(0, 1); // frame_id_numbers_present_flag
    s.put(0, 3); // 128x128, filter intra, intra edge
    s.put(0, 4); // interintra, masked, warped, dual filter
    s.put(0, 1); // enable_order_hint
    s.put(0, 1); // seq_choose_screen_content_tools
    s.put(0, 1); // seq_force_screen_content_tools = 0
    s.put(0, 3); // superres, cdef, restoration
    fm.color(&mut s);
    s.put(u32::from(fm.grain), 1);
    s.put(1, 1);
    obu(1, &s.bytes)
}

/// A shown key frame (refresh all) or a shown intra-only frame refreshing `refresh`.
fn full_frame(w: u32, h: u32, fm: Fmt, intra_only_refresh: Option<u8>, bytes_per_tile: usize) -> Vec<u8> {
    let (cl, rl, n) = tiles(w, h);
    let mut f = Bits::default();
    f.put(0, 1); // show_existing_frame
    f.put(if intra_only_refresh.is_some() { 2 } else { 0 }, 2); // frame_type
    f.put(1, 1); // show_frame
    if intra_only_refresh.is_some() {
        f.put(0, 1); // error_resilient_mode
    }
    f.put(0, 1); // disable_cdf_update
    f.put(0, 1); // frame_size_override_flag
    if let Some(r) = intra_only_refresh {
        f.put(u32::from(r), 8); // refresh_frame_flags
    }
    f.put(0, 1); // render_and_frame_size_different
    f.put(1, 1); // disable_frame_end_update_cdf
    f.put(1, 1); // uniform_tile_spacing_flag
    f.put(0, 2);
    if n > 1 {
        f.put(0, (cl + rl) as usize);
        f.put(3, 2);
    }
    fm.tail(&mut f);
    let mut frame = f.bytes;
    tile_group(&mut frame, n, bytes_per_tile);
    let mut out = obu(2, &[]);
    out.extend(obu(6, &frame));
    out
}

fn seq_for(fm: Fmt) -> SequenceHeader {
    let mut s = SequenceHeader::default();
    s.color.bit_depth = fm.depth as u8;
    s.color.mono_chrome = fm.mono;
    s.color.num_planes = fm.planes() as usize;
    let (ssx, ssy) = match (fm.profile, fm.depth, fm.mono) {
        (0, _, _) => (1, 1),
        (1, _, _) => (0, 0),
        (_, 12, false) => (0, 0),
        _ => (1, 0),
    };
    s.color.subsampling_x = ssx;
    s.color.subsampling_y = ssy;
    s.film_grain_params_present = fm.grain;
    s
}

/// Peak resident memory of this process, in bytes, since the last `reset_peak`.
fn peak_rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

fn reset_peak() -> bool {
    std::fs::write("/proc/self/clear_refs", "5").is_ok()
}

/// Decode eight distinct references of `w` x `h` and return the growth of the peak resident memory
/// (None when the kernel does not offer the numbers).
fn peak_of_eight_references(fm: Fmt, w: u32, h: u32) -> Option<u64> {
    let mut units = vec![full_sequence(w, h, fm).into_iter().chain(full_frame(w, h, fm, None, 64)).collect::<Vec<u8>>()];
    for i in 1..8u8 {
        units.push(full_frame(w, h, fm, Some(1 << i), 64));
    }
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !reset_peak() {
        return None;
    }
    let before = peak_rss()?;
    let mut dec = Decoder::with_threads(1);
    let mut pictures = 0;
    for (i, unit) in units.iter().enumerate() {
        match dec.decode_pts(unit, i as i64) {
            Ok(p) => pictures += p.len(),
            Err(e) => panic!("{w}x{h} {fm:?}: unit {i} failed: {e}"),
        }
    }
    pictures += dec.flush_result().map(|p| p.len()).unwrap_or_default();
    assert_eq!(pictures, 8, "{w}x{h} {fm:?}: every frame is shown");
    drop(dec);
    Some(peak_rss()?.saturating_sub(before))
}

const FORMATS: [Fmt; 5] = [
    Fmt { profile: 0, depth: 8, mono: false, grain: false },
    Fmt { profile: 0, depth: 10, mono: false, grain: true },
    Fmt { profile: 0, depth: 10, mono: true, grain: false },
    Fmt { profile: 2, depth: 10, mono: false, grain: false },
    Fmt { profile: 2, depth: 12, mono: false, grain: true },
];

fn check(w: u32, h: u32) {
    for fm in FORMATS {
        let seq = seq_for(fm);
        assert!(picture_fits_budget(&seq, w, h, DEFAULT_MEMORY_BUDGET), "{w}x{h} {fm:?} must fit the default budget");
        let estimate = memory_needed(&seq, w, h);
        let Some(peak) = peak_of_eight_references(fm, w, h) else {
            eprintln!("peak resident memory is not available here; skipping {w}x{h} {fm:?}");
            continue;
        };
        eprintln!("{w}x{h} {fm:?}: peak {} MB, estimate {} MB", peak / 1_000_000, estimate / 1_000_000);
        assert!(peak <= estimate, "{w}x{h} {fm:?}: peak {peak} bytes is over the estimate {estimate}");
    }
}

/// 720p, every layout: the estimate is an upper bound on what one thread really holds.
#[test]
fn eight_references_at_720p_stay_under_the_estimate() {
    check(1280, 720);
}

/// The largest 16:9 picture (width in steps of 64) the 1.5 GiB budget admits in 4:2:0, which is
/// 7424 x 4176: eight references of it stay under the budget itself. About 1 GB and some seconds, so
/// it runs on request: `cargo test -p soundcraft-video --features av1-decode --test av1_peak -- --ignored`.
#[test]
#[ignore = "allocates about 1 GB"]
fn eight_references_at_the_largest_admitted_picture_stay_under_the_budget() {
    let fm = FORMATS[0];
    let peak = peak_of_eight_references(fm, 7424, 4176);
    if let Some(peak) = peak {
        assert!(peak <= DEFAULT_MEMORY_BUDGET, "peak {peak} bytes is over the 1.5 GiB budget");
    }
}
