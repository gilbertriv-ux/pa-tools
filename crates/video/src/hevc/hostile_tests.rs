//! Hostile input. Every value a stream codes (parameter sets, slice headers, NAL lengths, slice
//! data) is untrusted: the decoder returns errors, never panics, never allocates more than its
//! budget. The tests build parameter sets and slice headers field by field with an extreme in one
//! place, and mutate real streams (the synthetic CABAC streams and the committed samples).
//!
//! Debug builds check integer overflow, so an unchecked `read_ue()? + N` is a panic here; the
//! decoder turns a panic during a picture into `Error::Internal`, and these tests fail on that error
//! as well as on a panic that escapes.

use crate::bitstream::{BitWriter, annexb_nals};
use crate::hevc::params::{Pps, Sps};
use crate::hevc::slice::{NalHeader, SliceHeader};
use crate::hevc::synth_tests::{nal, seed_streams};
use crate::hevc::{Decoder, Error};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use build::{PpsF, SliceF, SpsF, TilesF};

/// Builders of parameter sets and slice headers with every field settable.
mod build {
    use crate::bitstream::BitWriter;
    use crate::hevc::synth_tests::ptl;
    use std::rc::Rc;

    /// Writes extra syntax into a parameter set.
    pub type Extra = Rc<dyn Fn(&mut BitWriter)>;

    /// An SPS. The defaults are the synthetic streams' SPS: 96x64, 4:2:0 8-bit, CTB 16, PCM, long-term
    /// references.
    #[derive(Clone)]
    pub struct SpsF {
        pub chroma: u32,
        pub width: u32,
        pub height: u32,
        pub conf: Option<[u32; 4]>,
        pub bd_luma: u32,
        pub bd_chroma: u32,
        pub log2_poc: u32,
        pub dpb: u32,
        pub reorder: u32,
        pub latency: u32,
        pub min_cb: u32,
        pub diff_cb: u32,
        pub min_tb: u32,
        pub diff_tb: u32,
        pub th_inter: u32,
        pub th_intra: u32,
        pub pcm: Option<[u32; 2]>,
        pub num_st: Option<u32>,
        pub st: Vec<Extra>,
        pub lt_sps: Vec<(u32, bool)>,
        pub tmvp: bool,
    }

    impl Default for SpsF {
        fn default() -> Self {
            SpsF {
                chroma: 1,
                width: 96,
                height: 64,
                conf: None,
                bd_luma: 0,
                bd_chroma: 0,
                log2_poc: 4,
                dpb: 4,
                reorder: 0,
                latency: 0,
                min_cb: 0,
                diff_cb: 1,
                min_tb: 0,
                diff_tb: 2,
                th_inter: 0,
                th_intra: 0,
                pcm: Some([0, 1]),
                num_st: None,
                st: Vec::new(),
                lt_sps: Vec::new(),
                tmvp: false,
            }
        }
    }

    impl SpsF {
        pub fn rbsp(&self) -> Vec<u8> {
            let mut w = BitWriter::new();
            w.write_bits(0, 4);
            w.write_bits(0, 3);
            w.write_bits(1, 1);
            ptl(&mut w);
            w.write_ue(0); // sps id
            w.write_ue(self.chroma);
            if self.chroma == 3 {
                w.write_bits(0, 1);
            }
            w.write_ue(self.width);
            w.write_ue(self.height);
            w.write_bits(self.conf.is_some() as u32, 1);
            if let Some(c) = self.conf {
                c.iter().for_each(|&v| w.write_ue(v));
            }
            w.write_ue(self.bd_luma);
            w.write_ue(self.bd_chroma);
            w.write_ue(self.log2_poc);
            w.write_bits(1, 1);
            w.write_ue(self.dpb);
            w.write_ue(self.reorder);
            w.write_ue(self.latency);
            w.write_ue(self.min_cb);
            w.write_ue(self.diff_cb);
            w.write_ue(self.min_tb);
            w.write_ue(self.diff_tb);
            w.write_ue(self.th_inter);
            w.write_ue(self.th_intra);
            w.write_bits(0, 3); // scaling lists, amp, sao
            w.write_bits(self.pcm.is_some() as u32, 1);
            if let Some([min, diff]) = self.pcm {
                w.write_bits(7, 4);
                w.write_bits(7, 4);
                w.write_ue(min);
                w.write_ue(diff);
                w.write_bits(0, 1);
            }
            w.write_ue(self.num_st.unwrap_or(self.st.len() as u32));
            self.st.iter().for_each(|f| f(&mut w));
            w.write_bits(1, 1); // long_term_ref_pics_present
            w.write_ue(self.lt_sps.len() as u32);
            for &(lsb, used) in &self.lt_sps {
                w.write_bits(lsb, self.log2_poc + 4);
                w.write_bit(used);
            }
            w.write_bit(self.tmvp);
            w.write_bits(0, 1); // strong intra smoothing
            w.write_bits(0, 1); // vui
            w.write_bits(0, 1); // extensions
            w.rbsp_trailing();
            w.finish()
        }
    }

    /// Tile syntax of a PPS: columns minus 1, rows minus 1, uniform spacing, explicit widths and heights minus 1.
    #[derive(Clone)]
    pub struct TilesF {
        pub cols: u32,
        pub rows: u32,
        pub uniform: bool,
        pub widths: Vec<u32>,
        pub heights: Vec<u32>,
    }

    /// A PPS, defaulting to the synthetic streams' PPS 1 (list modification on).
    #[derive(Clone)]
    pub struct PpsF {
        pub id: u32,
        pub num_ref: [u32; 2],
        pub init_qp: i32,
        pub cu_qp_delta_depth: Option<u32>,
        pub cb_offset: i32,
        pub weighted_pred: bool,
        pub weighted_bipred: bool,
        pub tiles: Option<TilesF>,
        pub entropy_sync: bool,
        pub lists_mod: bool,
        pub merge_level: u32,
    }

    impl Default for PpsF {
        fn default() -> Self {
            PpsF {
                id: 1,
                num_ref: [0, 0],
                init_qp: 0,
                cu_qp_delta_depth: None,
                cb_offset: 0,
                weighted_pred: false,
                weighted_bipred: false,
                tiles: None,
                entropy_sync: false,
                lists_mod: true,
                merge_level: 0,
            }
        }
    }

    impl PpsF {
        pub fn rbsp(&self) -> Vec<u8> {
            let mut w = BitWriter::new();
            w.write_ue(self.id);
            w.write_ue(0);
            w.write_bits(1, 1); // dependent slice segments
            w.write_bits(0, 1);
            w.write_bits(0, 3);
            w.write_bits(0, 2); // sign hiding, cabac_init_present
            w.write_ue(self.num_ref[0]);
            w.write_ue(self.num_ref[1]);
            w.write_se(self.init_qp);
            w.write_bits(0, 2); // constrained intra, transform skip
            w.write_bits(self.cu_qp_delta_depth.is_some() as u32, 1);
            if let Some(d) = self.cu_qp_delta_depth {
                w.write_ue(d);
            }
            w.write_se(self.cb_offset);
            w.write_se(0);
            w.write_bits(0, 1); // slice chroma qp offsets
            w.write_bit(self.weighted_pred);
            w.write_bit(self.weighted_bipred);
            w.write_bits(0, 1); // transquant bypass
            w.write_bit(self.tiles.is_some());
            w.write_bit(self.entropy_sync);
            if let Some(t) = &self.tiles {
                w.write_ue(t.cols);
                w.write_ue(t.rows);
                w.write_bit(t.uniform);
                if !t.uniform {
                    t.widths.iter().for_each(|&v| w.write_ue(v));
                    t.heights.iter().for_each(|&v| w.write_ue(v));
                }
                w.write_bits(1, 1);
            }
            w.write_bits(1, 1); // loop filter across slices
            w.write_bits(0, 1); // deblocking control
            w.write_bits(0, 1); // scaling list
            w.write_bit(self.lists_mod);
            w.write_ue(self.merge_level);
            w.write_bits(0, 2);
            w.rbsp_trailing();
            w.finish()
        }
    }

    /// A non-IDR slice segment header against the default SPS (8-bit POC lsb) and a PPS.
    #[derive(Clone)]
    pub struct SliceF {
        pub pps_id: u32,
        /// 0 = B, 1 = P, 2 = I.
        pub slice_type: u32,
        /// delta_poc_s0_minus1 of the pictures the slice refers to (all used by the current picture).
        pub refs: Vec<u32>,
        /// num_long_term_sps (written only when the SPS lists long-term pictures) and num_long_term_pics.
        pub num_lt: (u32, u32),
        pub num_ref_override: Option<[u32; 2]>,
        pub pwt: Option<Extra>,
        pub five_minus: u32,
        pub qp_delta: i32,
        /// Entry points: (count, offset_len_minus1), written when the PPS has tiles or WPP.
        pub entry: (u32, u32),
        /// Whether the SPS lists long-term pictures (the slice then codes num_long_term_sps).
        pub sps_has_lt: bool,
    }

    impl Default for SliceF {
        fn default() -> Self {
            SliceF {
                pps_id: 1,
                slice_type: 1,
                refs: vec![0],
                num_lt: (0, 0),
                num_ref_override: Some([0, 0]),
                pwt: None,
                five_minus: 0,
                qp_delta: 0,
                entry: (0, 0),
                sps_has_lt: false,
            }
        }
    }

    impl SliceF {
        pub fn rbsp(&self, tiles_or_wpp: bool) -> Vec<u8> {
            let mut w = BitWriter::new();
            w.write_bit(true); // first_slice_segment_in_pic_flag
            w.write_ue(self.pps_id);
            w.write_ue(self.slice_type);
            w.write_bits(5, 8); // poc lsb
            w.write_bit(false); // short_term_ref_pic_set_sps_flag
            w.write_ue(self.refs.len() as u32);
            w.write_ue(0);
            for &d in &self.refs {
                w.write_ue(d);
                w.write_bit(true);
            }
            if self.sps_has_lt {
                w.write_ue(self.num_lt.0);
            }
            w.write_ue(self.num_lt.1);
            if self.slice_type != 2 {
                match self.num_ref_override {
                    Some([l0, l1]) => {
                        w.write_bit(true);
                        w.write_ue(l0);
                        if self.slice_type == 0 {
                            w.write_ue(l1);
                        }
                    }
                    None => w.write_bit(false),
                }
                if self.refs.len() > 1 {
                    w.write_bit(false); // ref_pic_list_modification_flag_l0
                    if self.slice_type == 0 {
                        w.write_bit(false);
                    }
                }
                if self.slice_type == 0 {
                    w.write_bit(false); // mvd_l1_zero_flag
                }
                if let Some(f) = &self.pwt {
                    f(&mut w);
                }
                w.write_ue(self.five_minus);
            }
            w.write_se(self.qp_delta);
            w.write_bit(true); // slice_loop_filter_across_slices_enabled_flag
            if tiles_or_wpp {
                w.write_ue(self.entry.0);
                if self.entry.0 > 0 {
                    w.write_ue(self.entry.1);
                    for _ in 0..self.entry.0.min(4) {
                        w.write_bits(0, 32);
                    }
                }
            }
            w.write_bit(true);
            w.align_zero();
            w.finish()
        }
    }
}

fn parse_sps(f: &SpsF) -> crate::hevc::Result<Sps> {
    Sps::parse(&f.rbsp())
}

fn parse_pps(f: &PpsF) -> crate::hevc::Result<Pps> {
    Pps::parse(&f.rbsp())
}

fn slice_header(s: &SliceF, pps: &PpsF, sps: &SpsF) -> crate::hevc::Result<SliceHeader> {
    let (p, q) = (Arc::new(parse_pps(pps)?), Arc::new(parse_sps(sps)?));
    let hdr = NalHeader { nal_type: 1, layer_id: 0, temporal_id: 0 };
    let tw = pps.tiles.is_some() || pps.entropy_sync;
    SliceHeader::parse(&s.rbsp(tw), hdr, |_| Ok((p.clone(), q.clone())), None).map(|(h, ..)| h)
}

/// Run `f`, failing the test if it panics.
fn no_panic<T>(what: &str, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => panic!("{what}: panicked"),
    }
}

const X: u32 = u32::MAX;

fn uniform_tiles(cols: u32, rows: u32) -> TilesF {
    TilesF { cols, rows, uniform: true, widths: vec![], heights: vec![] }
}

// ---- sequence parameter sets ----

#[test]
fn the_default_parameter_sets_and_slice_header_are_valid() {
    let sps = SpsF::default();
    assert!(parse_sps(&sps).unwrap().check_supported().is_ok());
    assert!(parse_pps(&PpsF::default()).is_ok());
    let sh = slice_header(&SliceF::default(), &PpsF::default(), &sps).unwrap();
    assert_eq!((sh.num_ref_idx[0], sh.max_num_merge_cand, sh.qp_delta), (1, 5, 0));
}

#[test]
fn sps_fields_at_the_top_of_their_range_are_errors() {
    type Set = fn(&mut SpsF, u32);
    let fields: [(&str, Set); 17] = [
        ("chroma_format_idc", |s, v| s.chroma = v),
        ("width", |s, v| s.width = v),
        ("height", |s, v| s.height = v),
        ("bit_depth_luma_minus8", |s, v| s.bd_luma = v),
        ("bit_depth_chroma_minus8", |s, v| s.bd_chroma = v),
        ("log2_max_pic_order_cnt_lsb_minus4", |s, v| s.log2_poc = v),
        ("sps_max_dec_pic_buffering_minus1", |s, v| s.dpb = v),
        ("sps_max_num_reorder_pics", |s, v| s.reorder = v),
        ("log2_min_luma_coding_block_size_minus3", |s, v| s.min_cb = v),
        ("log2_diff_max_min_luma_coding_block_size", |s, v| s.diff_cb = v),
        ("log2_min_luma_transform_block_size_minus2", |s, v| s.min_tb = v),
        ("log2_diff_max_min_luma_transform_block_size", |s, v| s.diff_tb = v),
        ("max_transform_hierarchy_depth_inter", |s, v| s.th_inter = v),
        ("max_transform_hierarchy_depth_intra", |s, v| s.th_intra = v),
        ("log2_min_pcm_luma_coding_block_size_minus3", |s, v| s.pcm = Some([v, 1])),
        ("log2_diff_max_min_pcm_luma_coding_block_size", |s, v| s.pcm = Some([0, v])),
        ("num_short_term_ref_pic_sets", |s, v| s.num_st = Some(v)),
    ];
    for (name, set) in fields {
        for v in [X, X - 1, 1 << 31, 1 << 16, 17, 33] {
            let mut s = SpsF::default();
            set(&mut s, v);
            let r = no_panic(name, || parse_sps(&s));
            assert!(r.is_err(), "{name} = {v} should be an error");
        }
    }
}

#[test]
fn a_conformance_window_at_the_top_of_the_range_crops_inside_the_picture() {
    for conf in [[X, X, X, X], [X, 0, 0, 0], [0, X, 0, 0], [0, 0, X, 0], [0, 0, 0, X], [1 << 31, 1 << 31, 1 << 31, 1 << 31], [100, 100, 100, 100]] {
        let s = SpsF { conf: Some(conf), ..Default::default() };
        let sps = parse_sps(&s).expect("a conformance window is not validated against the picture");
        let (x, y, w, h) = no_panic("crop_rect", || sps.crop_rect());
        assert!(w >= 1 && h >= 1 && x < sps.width && y < sps.height && x + w <= sps.width && y + h <= sps.height, "{conf:?}: {x},{y} {w}x{h}");
    }
}

#[test]
fn the_latency_limit_at_the_top_of_the_range_does_not_overflow() {
    // sps_max_latency_increase_plus1 = 2^32 - 1 and a reorder depth: SpsMaxLatencyPictures overflows u32
    let s = SpsF { reorder: 4, latency: X, ..Default::default() };
    let mut stream = crate::hevc::synth_tests::vps();
    stream.extend(nal(33, &s.rbsp()));
    stream.extend(nal(34, &PpsF::default().rbsp()));
    let mut dec = Decoder::with_threads(1);
    no_panic("latency", || dec.decode(&stream, 0)).expect("parameter sets only");
    // and through a picture: take a real stream and swap its SPS
    let (_, real) = seed_streams().remove(0);
    let sps_nal = annexb_nals(&real).into_iter().find(|n| (n[0] >> 1) & 0x3f == 33).map(<[u8]>::to_vec).expect("an SPS");
    let hostile = nal(33, &SpsF { latency: X, reorder: 2, ..Default::default() }.rbsp());
    let mut patched = Vec::new();
    for n in annexb_nals(&real) {
        patched.extend([0, 0, 0, 1]);
        patched.extend(if n == &sps_nal[..] { hostile[4..].to_vec() } else { n.to_vec() });
    }
    let mut dec = Decoder::with_threads(1);
    let r = no_panic("latency stream", || dec.decode(&patched, 0));
    assert!(!matches!(r, Err(Error::Internal(_))) && dec.take_error().is_none_or(|e| !matches!(e, Error::Internal(_))));
}

// ---- short-term reference picture sets ----

#[test]
fn inter_rps_and_delta_pocs_at_the_top_of_the_range_are_errors() {
    use std::rc::Rc;
    let explicit: build::Extra = Rc::new(|w: &mut BitWriter| {
        w.write_ue(1); // num_negative_pics
        w.write_ue(0);
        w.write_ue(0); // delta_poc_s0_minus1
        w.write_bit(true);
    });
    let inter = |sign: bool, abs: u32| -> build::Extra {
        Rc::new(move |w: &mut BitWriter| {
            w.write_bit(true); // inter_ref_pic_set_prediction_flag
            w.write_bit(sign);
            w.write_ue(abs); // abs_delta_rps_minus1
            for _ in 0..2 {
                w.write_bit(true); // used_by_curr_pic_flag
            }
        })
    };
    // abs_delta_rps_minus1 of u32::MAX, 2^31 - 1 (+1 overflowed i32) and 2^15
    for abs in [X, X - 1, (1 << 31) - 1, 1 << 31, 1 << 15] {
        let s = SpsF { st: vec![explicit.clone(), inter(false, abs)], ..Default::default() };
        assert!(no_panic("abs_delta_rps", || parse_sps(&s)).is_err(), "abs_delta_rps_minus1 = {abs}");
    }
    // the same set with sane values parses
    let ok = SpsF { st: vec![explicit.clone(), inter(true, 0)], ..Default::default() };
    assert!(parse_sps(&ok).is_ok());
    for (name, neg, pos) in [("num_negative_pics", X, 0), ("num_positive_pics", 0, X), ("sum", 9, 9)] {
        let s = SpsF {
            st: vec![Rc::new(move |w: &mut BitWriter| {
                w.write_ue(neg);
                w.write_ue(pos);
            })],
            ..Default::default()
        };
        assert!(no_panic(name, || parse_sps(&s)).is_err(), "{name}");
    }
    for d in [X, 1 << 31, 1 << 15] {
        let s = SpsF {
            st: vec![Rc::new(move |w: &mut BitWriter| {
                w.write_ue(1);
                w.write_ue(1);
                w.write_ue(d); // delta_poc_s0_minus1
                w.write_bit(true);
                w.write_ue(d); // delta_poc_s1_minus1
                w.write_bit(true);
            })],
            ..Default::default()
        };
        assert!(no_panic("delta_poc", || parse_sps(&s)).is_err(), "delta_poc_minus1 = {d}");
    }
}

// ---- picture parameter sets ----

#[test]
fn pps_fields_at_the_top_of_their_range_are_errors() {
    type Set = fn(&mut PpsF, u32);
    let fields: [(&str, Set); 7] = [
        ("pps_id", |p, v| p.id = v),
        ("num_ref_idx_l0_default_active_minus1", |p, v| p.num_ref[0] = v),
        ("num_ref_idx_l1_default_active_minus1", |p, v| p.num_ref[1] = v),
        ("diff_cu_qp_delta_depth", |p, v| p.cu_qp_delta_depth = Some(v)),
        ("log2_parallel_merge_level_minus2", |p, v| p.merge_level = v),
        ("num_tile_columns_minus1", |p, v| p.tiles = Some(uniform_tiles(v, 0))),
        ("num_tile_rows_minus1", |p, v| p.tiles = Some(uniform_tiles(0, v))),
    ];
    for (name, set) in fields {
        for v in [X, X - 1, 1 << 31, 1 << 16] {
            let mut p = PpsF::default();
            set(&mut p, v);
            let r = no_panic(name, || parse_pps(&p));
            assert!(r.is_err(), "{name} = {v} should be an error");
        }
    }
    for qp in [i32::MAX, i32::MIN, i32::MAX - 26, -75, 26] {
        let p = PpsF { init_qp: qp, ..Default::default() };
        assert!(no_panic("init_qp_minus26", || parse_pps(&p)).is_err(), "init_qp_minus26 = {qp}");
    }
    for off in [i32::MAX, i32::MIN, 13, -13] {
        let p = PpsF { cb_offset: off, ..Default::default() };
        assert!(no_panic("pps_cb_qp_offset", || parse_pps(&p)).is_err(), "{off}");
    }
    // explicit tile sizes at the top of the range saturate and are then refused against the picture
    let p = PpsF { tiles: Some(TilesF { cols: 1, rows: 0, uniform: false, widths: vec![X], heights: vec![] }), ..Default::default() };
    let pps = parse_pps(&p).expect("sizes are checked against the SPS");
    let sps = parse_sps(&SpsF::default()).unwrap();
    assert!(no_panic("layout", || crate::hevc::params::Layout::new(&sps, &pps)).is_err());
}

// ---- slice segment headers ----

#[test]
fn slice_header_fields_at_the_top_of_their_range_are_errors() {
    let sps = SpsF::default();
    let pps = PpsF::default();
    let cases: Vec<(&str, SliceF)> = vec![
        ("num_ref_idx_l0_active_minus1", SliceF { num_ref_override: Some([X, 0]), ..Default::default() }),
        ("num_ref_idx_l0_active_minus1 = 2^31", SliceF { num_ref_override: Some([1 << 31, 0]), ..Default::default() }),
        ("num_ref_idx_l1_active_minus1", SliceF { slice_type: 0, num_ref_override: Some([0, X]), ..Default::default() }),
        ("num_long_term_pics", SliceF { num_lt: (0, X), ..Default::default() }),
        ("five_minus_max_num_merge_cand", SliceF { five_minus: X, ..Default::default() }),
        ("slice_qp_delta max", SliceF { qp_delta: i32::MAX, ..Default::default() }),
        ("slice_qp_delta min", SliceF { qp_delta: i32::MIN, ..Default::default() }),
        ("slice_type", SliceF { slice_type: X, ..Default::default() }),
        ("pps id", SliceF { pps_id: X, ..Default::default() }),
        ("delta_poc_s0_minus1", SliceF { refs: vec![X], ..Default::default() }),
    ];
    for (name, s) in cases {
        let r = no_panic(name, || slice_header(&s, &pps, &sps));
        assert!(r.is_err(), "{name} should be an error");
    }
}

#[test]
fn long_term_picture_counts_that_overflow_when_added_are_errors() {
    // the SPS lists one long-term picture, so the slice codes num_long_term_sps: 1 + (2^32 - 1) wrapped
    let sps = SpsF { lt_sps: vec![(0, true)], ..Default::default() };
    for lt in [(1, X), (1, X - 1), (0, X), (X, X)] {
        let s = SliceF { num_lt: lt, sps_has_lt: true, ..Default::default() };
        assert!(no_panic("num_long_term", || slice_header(&s, &PpsF::default(), &sps)).is_err(), "{lt:?}");
    }
}

#[test]
fn entry_point_counts_and_lengths_at_the_top_of_their_range_are_errors() {
    let sps = SpsF::default();
    let pps = PpsF { tiles: Some(TilesF { cols: 1, rows: 0, uniform: true, widths: vec![], heights: vec![] }), ..Default::default() };
    for entry in [(X, 0), (1, X), (1, X - 1), (1, 32), (1 << 20, 0)] {
        let s = SliceF { entry, ..Default::default() };
        assert!(no_panic("entry points", || slice_header(&s, &pps, &sps)).is_err(), "{entry:?}");
    }
    // an honest count parses and is skipped without being kept
    let s = SliceF { entry: (2, 31), ..Default::default() };
    assert!(slice_header(&s, &pps, &sps).is_ok());
}

#[test]
fn weighted_prediction_denominators_at_the_top_of_the_range_are_errors() {
    use std::rc::Rc;
    let sps = SpsF::default();
    let pps = PpsF { weighted_pred: true, ..Default::default() };
    let pwt = |luma: u32, delta: i32| -> Option<build::Extra> {
        Some(Rc::new(move |w: &mut BitWriter| {
            w.write_ue(luma); // luma_log2_weight_denom
            w.write_se(delta); // delta_chroma_log2_weight_denom
        }))
    };
    for (luma, delta) in [(0, i32::MAX), (7, i32::MAX), (0, i32::MIN), (X, 0), (8, 0), (3, 5)] {
        let s = SliceF { pwt: pwt(luma, delta), ..Default::default() };
        assert!(no_panic("pwt", || slice_header(&s, &pps, &sps)).is_err(), "denominators {luma} {delta}");
    }
}

// ---- sizes and memory ----

#[test]
fn absurd_picture_sizes_are_refused_before_anything_is_allocated() {
    let big = |width: u32, height: u32, depth: u32, dpb: u32| {
        let mut s = SpsF { width, height, bd_luma: depth, bd_chroma: depth, dpb, ..Default::default() };
        // keep the size a multiple of the 8x8 minimum coding block
        s.width -= s.width % 8;
        s.height -= s.height % 8;
        parse_sps(&s).map(|sps| sps.check_supported())
    };
    // beyond the SPS limit: a parse error
    assert!(big(16896, 64, 0, 0).is_err() && big(64, 16896, 0, 0).is_err() && big(X, X, 0, 0).is_err());
    // within the limit, beyond the picture cap or the memory budget: unsupported
    for (w, h, depth, dpb, what) in [
        (16888, 16888, 0, 0, "285 Mpixel"),
        (8192, 8200, 0, 0, "just over the cap"),
        (8192, 8192, 2, 15, "8K, 10-bit, 16 pictures"),
        (8192, 8192, 0, 15, "8K, 8-bit, 16 pictures"),
    ] {
        let r = big(w, h, depth, dpb).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(matches!(r, Err(Error::Unsupported(_))), "{what}: {r:?}");
    }
    // sizes people use pass
    for (w, h, depth, dpb) in [(1920, 1080, 0, 5), (3840, 2160, 2, 5), (4096, 2160, 2, 5), (8192, 4320, 0, 5), (8192, 8192, 0, 0)] {
        assert!(big(w, h, depth, dpb).unwrap().is_ok(), "{w}x{h} depth {} dpb {}", 8 + depth, dpb + 1);
    }
}

#[test]
fn a_decoder_fed_an_oversize_sequence_stops_with_unsupported() {
    let s = SpsF { width: 16888, height: 16888, ..Default::default() };
    let (_, real) = seed_streams().remove(0);
    let mut stream = crate::hevc::synth_tests::vps();
    stream.extend(nal(33, &s.rbsp()));
    stream.extend(nal(34, &PpsF { id: 0, ..Default::default() }.rbsp()));
    // the first slice NAL of a real stream (it names PPS 0, which is the hostile one's id)
    let slice = annexb_nals(&real).into_iter().find(|n| (n[0] >> 1) & 0x3f < 32).map(<[u8]>::to_vec).expect("a slice");
    stream.extend([0, 0, 0, 1]);
    stream.extend(slice);
    let mut dec = Decoder::with_threads(1);
    let r = no_panic("oversize", || dec.decode(&stream, 0));
    assert!(matches!(r, Err(Error::Unsupported(_))), "{r:?}");
}

#[test]
fn frame_size_accounting() {
    let sps = parse_sps(&SpsF { width: 64, height: 64, ..Default::default() }).unwrap();
    // 4:2:0 bytes plus one motion record per 16x16 block
    assert_eq!(sps.frame_bytes(), 64 * 64 * 3 / 2 + 16 * std::mem::size_of::<crate::hevc::picture::ColMv>() as u64);
    let deep = parse_sps(&SpsF { width: 64, height: 64, bd_luma: 2, bd_chroma: 2, ..Default::default() }).unwrap();
    assert!(deep.frame_bytes() > sps.frame_bytes() * 3 / 2);
    assert_eq!(sps.dpb_bytes(), sps.frame_bytes() * (5 + 2));
}

// ---- NAL framing ----

#[test]
fn length_prefixed_units_that_run_past_the_data_are_errors() {
    let mut dec = Decoder::with_threads(1);
    // hvcC with 4-byte NAL lengths and no parameter sets
    let mut hvcc = vec![1u8; 1];
    hvcc.extend([0u8; 20]);
    hvcc.push(3); // lengthSizeMinusOne = 3
    hvcc.push(0); // no arrays
    dec.configure_hvcc(&hvcc).expect("an empty record");
    assert_eq!(dec.nal_length_size(), Some(4));
    for au in [
        vec![0xff, 0xff, 0xff, 0xff, 1, 2, 3],
        vec![0x80, 0, 0, 0, 1],
        vec![0, 0, 0, 5, 1, 2],
        vec![0, 0],
        vec![0, 0, 0, 2, 0x26],
        vec![0, 0, 0, 0, 0, 0, 0, 0],
    ] {
        let r = no_panic("framing", || dec.decode(&au, 0));
        assert!(!matches!(r, Err(Error::Internal(_))));
    }
    assert!(no_panic("framing", || dec.decode(&[0xff, 0xff, 0xff, 0xff, 1, 2, 3], 0)).is_err());
    assert!(dec.decode(&[0, 0, 0, 5, 1, 2], 0).is_err());
    // zero-length units are skipped
    assert!(dec.decode(&[0, 0, 0, 0], 0).is_ok());
}

#[test]
fn a_record_cannot_start_a_picture() {
    // an hvcC array holding an IDR slice must not start decoding anything
    let mut hvcc = vec![1u8];
    hvcc.extend([0u8; 20]);
    hvcc.push(3);
    hvcc.push(1); // one array
    hvcc.push(19); // IDR_W_RADL
    hvcc.extend(1u16.to_be_bytes());
    let unit = [19u8 << 1, 1, 0x80, 0x01, 0x02, 0x03];
    hvcc.extend((unit.len() as u16).to_be_bytes());
    hvcc.extend(unit);
    let mut dec = Decoder::with_threads(1);
    dec.configure_hvcc(&hvcc).expect("record");
    assert!(dec.flush().is_empty());
    assert_eq!(dec.stats().pictures, 0);
    assert_eq!(dec.stats().slices_i, 0);
    // a record cut short is an error
    assert!(Decoder::with_threads(1).configure_hvcc(&hvcc[..hvcc.len() - 2]).is_err());
}

#[test]
fn a_damaged_video_parameter_set_is_ignored() {
    // the VPS plays no part in decoding, so garbage in its place changes nothing
    for (name, stream) in seed_streams() {
        let mut clean = Decoder::with_threads(1);
        let mut want = clean.decode(&stream, 0).expect(name);
        want.extend(clean.flush());
        let mut patched = Vec::new();
        for n in annexb_nals(&stream) {
            patched.extend([0, 0, 0, 1]);
            if (n[0] >> 1) & 0x3f == 32 {
                patched.extend([32u8 << 1, 1, 0xff, 0xff, 0xff, 0xff, 0xff]);
            } else {
                patched.extend(n);
            }
        }
        let mut dec = Decoder::with_threads(1);
        let mut got = dec.decode(&patched, 0).expect(name);
        got.extend(dec.flush());
        assert_eq!(got.len(), want.len(), "{name}");
        assert!(got.iter().zip(&want).all(|(a, b)| a.y == b.y && a.u == b.u && a.v == b.v), "{name}");
    }
}

// ---- mutation ----

/// The committed samples as Annex-B streams, beside the synthetic ones.
fn seeds() -> Vec<(String, Vec<u8>)> {
    use crate::isobmff::{CodecConfig, TrackKind};
    let mut out: Vec<(String, Vec<u8>)> = seed_streams().into_iter().map(|(n, s)| (format!("synth {n}"), s)).collect();
    out.push(("sample main8 (Annex-B)".into(), include_bytes!("../../tests/fixtures/hevc/hevc-main-8bit-64x64-4f.h265").to_vec()));
    for (name, bytes) in [
        ("sample main8 mp4", &include_bytes!("../../tests/fixtures/hevc/hevc-main-8bit-64x64-4f.mp4")[..]),
        ("sample main10 mp4", &include_bytes!("../../tests/fixtures/hevc/hevc-main10-64x64-4f.mp4")[..]),
        ("sample bframes mp4", &include_bytes!("../../tests/fixtures/hevc/hevc-main-8bit-64x64-bframes2-ref2-48f.mp4")[..]),
    ] {
        let src = bytes.to_vec();
        let mp4 = crate::isobmff::open(src.clone()).expect("mp4");
        let t = mp4.tracks.iter().position(|t| t.kind == TrackKind::Video).expect("video");
        let Some(CodecConfig::Hevc(cfg)) = mp4.tracks[t].codec() else { panic!("not HEVC") };
        let mut stream = Vec::new();
        let mut add = |n: &[u8]| {
            stream.extend([0, 0, 0, 1]);
            stream.extend(n);
        };
        for a in &cfg.arrays {
            a.nalus.iter().for_each(|n| add(n));
        }
        for s in 0..mp4.tracks[t].samples.len() {
            let data = mp4.read_sample(&src, t, s).expect("sample");
            let mut rest = &data[..];
            while rest.len() > 4 {
                let n = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
                add(&rest[4..4 + n]);
                rest = &rest[4 + n..];
            }
        }
        out.push((name.into(), stream));
    }
    out
}

/// xorshift, deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Feed `stream` to a fresh decoder, flush it, and check that nothing panicked (not even inside a
/// picture, where the decoder would report `Error::Internal`). True if it ran; a stream whose SPS asks
/// for a big picture is skipped, the size tests cover those.
fn decode_checked(what: &str, stream: &[u8]) -> bool {
    for n in annexb_nals(stream) {
        if n.len() > 2
            && (n[0] >> 1) & 0x3f == 33
            && let Ok(s) = Sps::parse(&crate::bitstream::unescape_rbsp(&n[2..]))
            && u64::from(s.width) * u64::from(s.height) > 1 << 22
        {
            return false;
        }
    }
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut dec = Decoder::with_threads(1);
        let first = dec.decode(stream, 0);
        let mut internal = matches!(first, Err(Error::Internal(_)));
        let pics = dec.flush();
        internal |= dec.take_error().is_some_and(|e| matches!(e, Error::Internal(_)));
        for p in first.iter().flatten().chain(pics.iter()) {
            // every picture that comes out is a whole one
            assert_eq!(p.y.len(), p.width as usize * p.height as usize);
            assert_eq!(p.u.len(), p.v.len());
        }
        internal
    }));
    match r {
        Ok(internal) => assert!(!internal, "{what}: a panic inside a picture (Error::Internal)"),
        Err(_) => panic!("{what}: panicked"),
    }
    true
}

fn iterations() -> usize {
    std::env::var("HEVC_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(120)
}

#[test]
fn mutated_and_truncated_streams_never_panic() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut ran = 0;
    for (name, seed) in seeds() {
        assert!(decode_checked(&format!("{name} unmutated"), &seed), "{name}");
        for it in 0..iterations() {
            let mut s = seed.clone();
            match it % 5 {
                0 => s.truncate(rng.below(seed.len() + 1)),
                1 => {
                    for _ in 0..1 + rng.below(8) {
                        let p = rng.below(s.len());
                        s[p] ^= (1 + rng.below(255)) as u8;
                    }
                }
                2 => {
                    // single bits
                    for _ in 0..1 + rng.below(4) {
                        let p = rng.below(s.len());
                        s[p] ^= 1 << rng.below(8);
                    }
                }
                3 => {
                    // overwrite a run with extremes (0x00 / 0xff runs make huge Exp-Golomb codes)
                    let p = rng.below(s.len());
                    let n = 1 + rng.below(6);
                    let v = [0x00, 0xff, 0x7f, 0x80][rng.below(4)];
                    for b in s.iter_mut().skip(p).take(n) {
                        *b = v;
                    }
                }
                _ => {
                    // duplicate or drop a stretch
                    let a = rng.below(s.len());
                    let b = (a + 1 + rng.below(200)).min(s.len());
                    if rng.below(2) == 0 {
                        let chunk = s[a..b].to_vec();
                        s.splice(a..a, chunk);
                    } else {
                        s.drain(a..b);
                    }
                }
            }
            ran += decode_checked(&format!("{name} iteration {it}"), &s) as usize;
        }
    }
    assert!(ran > 0);
}

/// Every byte of the parameter sets and the first slice header, set to a few extremes in turn.
#[test]
fn a_sweep_over_the_header_bytes_never_panics() {
    for (name, seed) in seeds() {
        // the bytes up to the end of the first slice NAL's header area
        let nals = annexb_nals(&seed);
        let first_slice = nals.iter().position(|n| (n[0] >> 1) & 0x3f < 32).unwrap_or(nals.len() - 1);
        let mut end = 0;
        for n in &nals[..=first_slice] {
            end = n.as_ptr() as usize - seed.as_ptr() as usize + n.len().min(24);
        }
        let end = end.min(seed.len()).min(260);
        for pos in 0..end {
            for v in [0x00, 0xff, 0x80, 0x7f, 0x01, seed[pos] ^ 0xff] {
                let mut s = seed.clone();
                s[pos] = v;
                decode_checked(&format!("{name} byte {pos} = {v:#x}"), &s);
            }
        }
    }
}

mod fuzz {
    use super::*;
    use proptest::prelude::*;

    fn config() -> ProptestConfig {
        ProptestConfig { cases: 96, failure_persistence: None, ..ProptestConfig::default() }
    }

    proptest! {
        #![proptest_config(config())]

        /// A few arbitrary byte changes anywhere in a real stream.
        #[test]
        fn arbitrary_byte_changes(seed in 0usize..9, edits in proptest::collection::vec((any::<u16>(), any::<u8>()), 0..12), cut in proptest::option::of(any::<u16>())) {
            let seeds = seeds();
            let (name, base) = &seeds[seed % seeds.len()];
            let mut s = base.clone();
            for (p, v) in edits {
                let n = s.len();
                s[p as usize % n] = v;
            }
            if let Some(c) = cut {
                s.truncate(c as usize % (s.len() + 1));
            }
            decode_checked(name, &s);
        }

        /// Arbitrary bytes as the slice data of a valid picture: the header parses, the CABAC runs on noise.
        #[test]
        fn noise_as_slice_data(seed in 0usize..6, data in proptest::collection::vec(any::<u8>(), 0..400), nal_type in prop_oneof![Just(19u8), Just(1u8), Just(21u8)]) {
            let seeds = seeds();
            let (name, base) = &seeds[seed % 6];
            let nals = annexb_nals(base);
            let first_slice = nals.iter().position(|n| (n[0] >> 1) & 0x3f < 32).unwrap_or(0);
            let mut s = Vec::new();
            for n in &nals[..first_slice] {
                s.extend([0, 0, 0, 1]);
                s.extend(*n);
            }
            // the real header of the first slice, then noise
            let hdr = &nals[first_slice];
            let keep = hdr.len().min(8);
            let mut unit = vec![nal_type << 1, 1];
            unit.extend(&hdr[2..keep]);
            unit.extend(&data);
            s.extend([0, 0, 0, 1]);
            s.extend(crate::bitstream::escape_rbsp(&unit));
            decode_checked(name, &s);
        }
    }
}
