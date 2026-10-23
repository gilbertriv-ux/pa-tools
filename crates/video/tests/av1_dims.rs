//! A few bytes must not make the AV1 decoder allocate gigabytes or crash: oversized sequences and
//! impossible headers are refused with an error. Hand-built (no encoder): a reduced still-picture
//! sequence header plus a lossless key frame whose tiles are one zero byte each. The stream builder
//! and the `PROFILE_FLIPPED` stream are from FilmCraft's `hostile_dims` test (MIT OR Apache-2.0).
#![cfg(feature = "av1-decode")]

use soundcraft_video::av1::{DEFAULT_MEMORY_BUDGET, Decoder, Error};

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
            if (value >> i) & 1 == 1
                && let Some(b) = self.bytes.last_mut()
            {
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

/// Temporal delimiter and the reduced still-picture sequence header of `w` x `h` (64x64 superblocks).
fn sequence(w: u32, h: u32) -> Vec<u8> {
    let mut s = Bits::default();
    s.put(0, 3); // seq_profile
    s.put(1, 1); // still_picture
    s.put(1, 1); // reduced_still_picture_header
    s.put(0, 5); // seq_level_idx
    s.put(15, 4); // frame_width_bits_minus_1
    s.put(15, 4); // frame_height_bits_minus_1
    s.put(w - 1, 16);
    s.put(h - 1, 16);
    s.put(0, 3 + 3 + 7 + 1); // sb size, filter intra, edge; superres, cdef, restoration; color config; film grain
    s.put(1, 1); // trailing bit
    let mut out = obu(2, &[]);
    out.extend(obu(1, &s.bytes));
    out
}

/// Sequence header plus a key frame OBU with the given payload (frame header bytes, then tile group).
fn stream_with_frame(w: u32, h: u32, frame: &[u8]) -> Vec<u8> {
    let mut out = sequence(w, h);
    out.extend(obu(6, frame));
    out
}

/// A lossless key frame whose uniformly spaced tiles are one zero byte each.
fn stream(w: u32, h: u32) -> Vec<u8> {
    let (sb_cols, sb_rows) = (w.div_ceil(64), h.div_ceil(64));
    let cols_log2 = tile_log2(64, sb_cols); // MAX_TILE_WIDTH / 64 superblocks
    let min_log2_tiles = cols_log2.max(tile_log2(2304, sb_rows * sb_cols));
    let rows_log2 = min_log2_tiles.saturating_sub(cols_log2);
    let tiles = (sb_cols.div_ceil(sb_cols.div_ceil(1 << cols_log2))) * (sb_rows.div_ceil(sb_rows.div_ceil(1 << rows_log2)));
    let mut f = Bits::default();
    f.put(0b0001, 4); // disable_cdf_update, allow_screen_content_tools, render_and_frame_size_different, uniform_tile_spacing_flag
    f.put(0, 2); // no increment of the tile column / row counts
    if tiles > 1 {
        f.put(0, (cols_log2 + rows_log2) as usize + 2); // context_update_tile_id, tile_size_bytes_minus_1
    }
    f.put(0, 8 + 4 + 1 + 1); // base_q_idx 0 (lossless), delta flags, using_qmatrix, segmentation_enabled, reduced_tx_set
    let mut frame = f.bytes;
    frame.push(0); // tile_start_and_end_present_flag
    for _ in 1..tiles {
        frame.extend([0, 0]); // tile_size_minus_1 = 0, one data byte
    }
    frame.extend([0, 0]);
    stream_with_frame(w, h, &frame)
}

fn decode(dec: &mut Decoder, data: &[u8]) -> Result<usize, Error> {
    dec.decode(data).and_then(|a| dec.flush_result().map(|b| a.len() + b.len()))
}

/// Refused as too large for the spec's own limits (level 6.3).
fn refused_by_level(w: u32, h: u32) {
    let r = decode(&mut Decoder::new(), &stream(w, h));
    assert!(matches!(r, Err(Error::Invalid(m)) if m.contains("frame size")), "{w}x{h} must be refused as too large, got {r:?}");
}

/// Within the spec's limits but more than the decoder is willing to hold.
fn refused_by_budget(w: u32, h: u32) {
    let r = decode(&mut Decoder::new(), &stream(w, h));
    assert!(matches!(r, Err(Error::Unsupported(m)) if m.contains("memory budget")), "{w}x{h} must be refused by the memory budget, got {r:?}");
}

#[test]
fn very_wide_or_tall_frames_are_refused() {
    refused_by_level(65536, 16);
    refused_by_level(16, 65536);
}

#[test]
fn huge_frames_are_refused_without_allocating() {
    refused_by_level(65536, 65536);
    refused_by_level(65535, 65535);
    refused_by_level(30000, 30000);
}

/// Frames past the level 6.x limits (width 16384, height 8704, area 35 651 584) are refused.
#[test]
fn frames_past_the_level_6_limits_are_refused() {
    refused_by_level(16384, 2177);
    refused_by_level(16384, 8704);
    refused_by_level(4096, 8705);
}

/// Frames inside the level limits whose references and working copies would need more than the
/// default 1.6 GiB (Gil, decision 5b) are refused up front, before any of it is allocated. (Full 8K UHD,
/// 7680 x 4320 in 4:2:0, is under it now: `adapt_av1.rs` and `av1::decoder` test that it is admitted.)
#[test]
fn frames_beyond_the_memory_budget_are_refused_fast() {
    let start = std::time::Instant::now();
    refused_by_budget(16384, 2176); // exactly MaxPicSize
    refused_by_budget(8192, 4352);
    refused_by_budget(8192, 4320); // 1.698 GiB estimated
    refused_by_budget(8192, 4096); // DCI 8K: 1.604 GiB estimated, 3.8 MB over
    assert!(start.elapsed().as_secs() < 5, "refusing must not allocate or decode");
}

#[test]
fn a_frame_that_fits_is_decoded_and_the_budget_is_adjustable() {
    // 4K UHD fits the default budget (about 450 MB of estimated state) and is only refused for bad data.
    let r = decode(&mut Decoder::new(), &stream(3840, 2160));
    assert!(!matches!(r, Err(Error::Unsupported(m)) if m.contains("memory budget")), "{r:?}");
    // A small frame decodes (lossless, empty tiles: grey picture).
    let mut dec = Decoder::with_threads(1);
    assert_eq!(decode(&mut dec, &stream(64, 48)), Ok(1));
    // A budget of zero refuses everything; a generous one accepts what the default refuses.
    let mut dec = Decoder::with_threads(1);
    dec.set_memory_budget(0);
    assert!(matches!(decode(&mut dec, &stream(64, 48)), Err(Error::Unsupported(_))));
    assert_eq!(DEFAULT_MEMORY_BUDGET, 1_717_986_918, "1.6 GiB");
}

/// A 64x48 4:2:0 key frame whose sequence profile bit was flipped to 2 (4:2:2): blocks such as 4x16 then have no
/// chroma size (BLOCK_INVALID) and used to index the size tables out of bounds.
const PROFILE_FLIPPED: &[u8] = &[
    0x12, 0x00, 0x0a, 0x0b, 0x42, 0x00, 0x00, 0x05, 0x55, 0x7f, 0xbc, 0x6a, 0xf9, 0x00, 0x40, 0x32, 0x81, 0x05, 0x14, 0x00, 0xa5, 0xa0, 0x40, 0xd9,
    0x52, 0xb9, 0xe7, 0x3c, 0xa6, 0xc1, 0xe4, 0xe9, 0xff, 0x6d, 0x12, 0xc3, 0xce, 0xac, 0x00, 0x92, 0x6f, 0x29, 0x6a, 0xee, 0x7e, 0x1a, 0x31, 0xfe,
    0xe6, 0xb8, 0x2e, 0xe2, 0xcf, 0x3d, 0xce, 0x24, 0xe6, 0x30, 0x03, 0xcd, 0x89, 0x49, 0x1f, 0xa7, 0x8a, 0x75, 0xb3, 0x93, 0x27, 0x7b, 0x2f, 0x39,
    0xc7, 0x60, 0xb7, 0xf8, 0x69, 0xbc, 0x53, 0x71, 0x30, 0x64, 0x20, 0x7e, 0xc1, 0x10, 0x17, 0xe4, 0xca, 0x34, 0xe9, 0xae, 0x47, 0x39, 0x68, 0x68,
    0xef, 0x33, 0x9f, 0x26, 0xe2, 0x3f, 0xf1, 0xef, 0x02, 0xb7, 0xfd, 0xb1, 0x78, 0x4c, 0x35, 0x31, 0x17, 0x27, 0x11, 0x2f, 0xd7, 0x25, 0x01, 0x30,
    0xa5, 0x7a, 0xaf, 0x9b, 0x5b, 0x3b, 0x8d, 0x9e, 0xd5, 0x23, 0x06, 0x22, 0x3a, 0xec, 0x57, 0xae, 0xdb, 0x4d, 0x00, 0x36, 0xff, 0xbd, 0x3b, 0xef,
    0x0a, 0xf0, 0x39, 0x2a, 0xa9, 0x2a, 0x56, 0x9d, 0x7e, 0xc2, 0x05, 0x46, 0x21, 0x8b, 0xd9, 0x45, 0x33, 0x59, 0x3c, 0x40, 0xe0, 0x2b, 0xec, 0x51,
    0x0a, 0xf2, 0xf0, 0xe5, 0x61, 0x99, 0xfd, 0x2e, 0xe5, 0x8c, 0x69, 0x72, 0x44, 0xb8, 0x92, 0x9e, 0x2e, 0x51, 0x0b, 0x8c, 0x16, 0x0f, 0xd9, 0xf2,
    0x47, 0xc4, 0x36, 0x9d, 0x54, 0xc6, 0x9f, 0x26, 0x68, 0xf7, 0xfa, 0x80, 0xab, 0x68, 0xd6, 0x61, 0xd2, 0x59, 0x4b, 0x07, 0xc4, 0x7a, 0xde, 0x37,
    0xfb, 0x02, 0xd5, 0x3d, 0xb0, 0xeb, 0x71, 0x1b, 0x07, 0x45, 0xdc, 0x03, 0x3b, 0x4f, 0xdf, 0x44, 0xa6, 0x28, 0xd4, 0xb0, 0xbb, 0x26, 0xa4, 0x4f,
    0xee, 0xbb, 0xda, 0x59, 0x69, 0xfc, 0xf2, 0x4f, 0xf6, 0x6c, 0x0c, 0xd9, 0xe2, 0xc0, 0xa7, 0x64, 0x96, 0xd5, 0x7b, 0x1a, 0x71, 0xe0, 0xcc, 0x9e,
    0xd7, 0xaf, 0xb9, 0x9a, 0x99, 0xcc, 0xe9, 0xa9, 0x79, 0xa3, 0xa8, 0xbc, 0xfe, 0xdb, 0xed, 0xa3, 0xd6, 0x7d, 0x31, 0xb0, 0x18, 0x3c, 0x9e, 0x55,
    0x0f, 0x1a, 0xdf, 0x5d, 0x8c, 0x33, 0x5f, 0x2d, 0x00, 0xfc, 0x9d, 0x55, 0xc2, 0xc8, 0x10, 0x25, 0xc8, 0x7f, 0xa6, 0x47, 0xbf, 0xab, 0xf3, 0x97,
    0xfb, 0xda, 0xd6, 0x6a, 0x14, 0x87, 0x9d, 0xef, 0x85, 0x12, 0x43, 0x31, 0xc4, 0x4c, 0x4c, 0x7a, 0x50, 0xd1, 0x87, 0xb8, 0x65, 0x2d, 0x3f, 0xf4,
    0xa3, 0x54, 0xea, 0x19, 0xa6, 0xfe, 0x70, 0x30, 0x52, 0x7e, 0xbb, 0x9c, 0x3f, 0x2d, 0x10, 0x4b, 0x43, 0x04, 0x6c, 0x03, 0xc7, 0x15, 0xa8, 0x8e,
    0x20, 0xde, 0x46, 0x9a, 0x12, 0xdf, 0xe5, 0xce, 0x08, 0x91, 0x3e, 0x74, 0xa4, 0x20, 0x78, 0x52, 0xfd, 0x4d, 0x9d, 0x5a, 0xca, 0xa2, 0xdc, 0xc8,
    0x95, 0xa6, 0x69, 0x3a, 0x1a, 0x18, 0x48, 0x76, 0x8c, 0xfa, 0x58, 0xa0, 0x0d, 0xf4, 0xbd, 0xe1, 0x05, 0x37, 0xe6, 0xbb, 0x60, 0xa1, 0x83, 0x40,
    0x08, 0xeb, 0x02, 0xa6, 0x6a, 0x7a, 0x16, 0x1f, 0xb3, 0xbb, 0x58, 0x66, 0x51, 0x79, 0xc7, 0x7a, 0xb2, 0xc2, 0x84, 0x69, 0x17, 0x4d, 0x27, 0xa7,
    0xa4, 0xd9, 0xc2, 0xb4, 0x03, 0x57, 0x53, 0x44, 0xb9, 0x8f, 0x48, 0xd2, 0x1c, 0x74, 0xd9, 0xdd, 0x9b, 0x88, 0xdc, 0xda, 0xef, 0x70, 0xee, 0xf1,
    0x04, 0xdf, 0x35, 0x39, 0x22, 0x49, 0xfe, 0x22, 0x9a, 0x0a, 0x33, 0x60, 0x49, 0xbe, 0x28, 0xfa, 0xdf, 0x71, 0xf6, 0x53, 0x87, 0x49, 0x01, 0x95,
    0xfa, 0x22, 0x94, 0x7c, 0x0e, 0xf6, 0x7d, 0x7e, 0x1d, 0x41, 0x8d, 0xc9, 0x1b, 0xf9, 0x53, 0xc9, 0xea, 0x46, 0xee, 0xb2, 0x80, 0x75, 0xe1, 0xca,
    0xa4, 0x0a, 0x9c, 0x2b, 0x7b, 0x18, 0x70, 0x56, 0x22, 0x76, 0xaf, 0x4d, 0x84, 0xdd, 0xad, 0x77, 0x0e, 0xea, 0x2e, 0xa9, 0xbf, 0x42, 0x95, 0xdf,
    0x17, 0x61, 0xae, 0x46, 0x20, 0xb4, 0x57, 0xca, 0xe0, 0x78, 0x86, 0xd0, 0x51, 0xae, 0xfe, 0x51, 0x53, 0x0f, 0x36, 0x64, 0x5f, 0xb4, 0x5c, 0xac,
    0x8b, 0x63, 0xe7, 0x5d, 0x98, 0x8e, 0x65, 0xe9, 0x93, 0xe4, 0x23, 0x10, 0xa3, 0x5b, 0x3b, 0x38, 0x5b, 0x8a, 0x7c, 0xe8, 0x60, 0x01, 0xc1, 0x0e,
    0x53, 0x5e, 0x0e, 0x81, 0x33, 0x63, 0x8d, 0x53, 0x82, 0xa6, 0x7b, 0xbd, 0x02, 0x58, 0x1f, 0xe9, 0x50, 0x75, 0x24, 0x64, 0x80, 0x47, 0x02, 0x9e,
    0x42, 0x28, 0xb0, 0xa2, 0xab, 0x8e, 0xd0, 0x81, 0xcc, 0xbd, 0x22, 0x87, 0x3f, 0xe4, 0x49, 0x4d, 0x5f, 0x6a, 0x5c, 0x5a, 0x5e, 0xe5, 0x30, 0x9d,
    0x7f, 0xc5, 0xaf, 0xce, 0x6c, 0x9c, 0xaf, 0x09, 0x96, 0x1a, 0x62, 0x42, 0x61, 0xa3, 0x1d, 0x1c, 0xe6, 0xcf, 0xa8, 0x90, 0x4d, 0x05, 0x28, 0xef,
    0x22, 0x81, 0x12, 0xa7, 0x70, 0x04, 0x0a, 0x0d, 0xe5, 0xa0, 0xd8,
];

#[test]
fn block_without_chroma_size_is_an_error_not_a_panic() {
    let mut dec = Decoder::new();
    let r = dec.decode(PROFILE_FLIPPED).and_then(|_| dec.flush_result());
    assert!(matches!(r, Err(Error::Invalid(m)) if m.contains("chroma size")), "got {:?}", r.as_ref().map(|p| p.len()));
}

/// Non-uniform tile spacing can ask for more than the 64 tile columns the spec allows (one superblock
/// each across a picture 65 superblocks wide).
#[test]
fn more_than_sixty_four_tile_columns_are_refused() {
    let mut f = Bits::default();
    f.put(0b0000, 4); // disable_cdf_update, allow_screen_content_tools, render_and_frame_size_different, uniform_tile_spacing_flag = 0
    for _ in 0..64 {
        f.put(0, 6); // width_in_sbs_minus_1 = ns(64) = 0: a one superblock column
    }
    // The 65th column spans the one remaining superblock (ns(1) reads no bits). Pad generously.
    f.put(0, 20);
    f.put(0, 20);
    let r = decode(&mut Decoder::new(), &stream_with_frame(65 * 64, 64, &f.bytes));
    assert!(matches!(r, Err(Error::Invalid(m)) if m.contains("too many tiles")), "got {r:?}");
}

/// Two tiles (a 128 wide picture split in two columns); `tile_start_and_end_present_flag` selects the range.
fn two_tile_frame(start_end: Option<(u32, u32)>) -> Vec<u8> {
    let mut f = Bits::default();
    f.put(0b0001, 4); // uniform_tile_spacing_flag = 1
    f.put(1, 1); // increment_tile_cols_log2: two columns, the most a 128 wide picture allows
    f.put(0, 1 + 2); // context_update_tile_id, tile_size_bytes_minus_1 = 0
    f.put(0, 8 + 4 + 1 + 1); // lossless, as `stream`
    let mut frame = f.bytes; // the frame header ends on a byte boundary, then the tile group starts
    let mut g = Bits::default();
    match start_end {
        Some((a, b)) => {
            g.put(1, 1);
            g.put(a, 1);
            g.put(b, 1);
        }
        None => g.put(0, 1),
    }
    frame.extend(g.bytes);
    frame.extend([0, 0, 0]); // tile 0: size byte (one data byte), tile 1: one data byte
    frame
}

#[test]
fn tile_groups_out_of_order_or_inverted_are_refused() {
    for range in [(1, 1), (1, 0)] {
        let r = decode(&mut Decoder::new(), &stream_with_frame(128, 64, &two_tile_frame(Some(range))));
        assert!(r.is_err(), "tile group {range:?} must be refused, got {r:?}");
    }
    // The same picture with one tile group holding both tiles gets past the tile group checks.
    for start_end in [None, Some((0, 1))] {
        let r = decode(&mut Decoder::new(), &stream_with_frame(128, 64, &two_tile_frame(start_end)));
        assert!(!matches!(r, Err(Error::Invalid(m)) if m.contains("tile group")), "{start_end:?}: {r:?}");
    }
}
