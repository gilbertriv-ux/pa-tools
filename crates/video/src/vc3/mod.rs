//! Clean-room VC-3 picture codec (SMPTE ST 2019-1:2016 and its Amendment 1:2023), the intra-frame
//! broadcast and post-production format also known by the names of its two profile families:
//! the fixed-raster HD profile (compression IDs 1235-1260) and the resolution-independent
//! profiles (1270-1274). Copied from FilmCraft's `filmcraft-dnx` (`storytold/filmcraft`, MIT OR
//! Apache-2.0, commit `f47a1f6`), which was written from the public standard; its constant tables
//! (Annex D weights, Annex E codewords) were extracted from the standard's text by the donor's
//! script. The deltas here are hardening (see the list below) and the neutral names; the on-screen
//! label of a stream is [`label`], e.g. "VC-3 (HQX, 10-bit)".
//!
//! - **Decoder** ([`decode_frame`]): every compression ID of the standard, including interlaced
//!   field and macroblock-adaptive coding, thin rasters and 4:4:4 RGB, and the LB, SQ, HQ, HQX
//!   and 444 profiles at 8, 10 and 12 bits in 4:2:0, 4:2:2 and 4:4:4, with DCT or lossless alpha.
//!   Macroblock scan lines decode in parallel with the `threads` feature (rayon).
//! - **Encoder** ([`Encoder`]): LB, SQ and HQ (8-bit 4:2:2), HQX (10- or 12-bit 4:2:2) and 444
//!   (10- or 12-bit), CBR at the standard's frame sizes, progressive. Used here to make test
//!   streams; the app does not write VC-3.
//!
//! ## Accuracy
//!
//! The donor measured the decoder against ffmpeg's: every sample within 2 levels, at most about 50
//! per million beyond 1, 87 to 99 % bit-identical (ffmpeg uses an integer inverse DCT, this
//! decoder the exact one of equation 8.3). `oracle.rs` repeats that comparison (ignored, needs
//! ffmpeg), and `fixture_tests.rs` checks a committed ffmpeg-made sample without it. In three
//! places the decoder follows ffmpeg's measured behaviour instead of the standard's text
//! ([`DecodeOptions::spec_dequant`] selects the text for the first): the reconstruction offset of
//! a quantised amplitude (3/4 of a step, not 1/2), the weights of compression ID 1271 (the ones
//! of 1241), and the 4:4:4 signalling (every 4:4:4 stream is R'G'B' except macroblocks flagged
//! `ACF`; planes are G, B, R).
//!
//! ## Hardening against hostile files
//!
//! Headers are untrusted: sizes are capped at 8192 on a side and at the crate's picture limit
//! (both fields of a field-coded frame counted) before any plane exists, every scan line must be
//! long enough for its macroblocks before planes are allocated, decoding fits a 1 GiB budget,
//! the HD profile refuses the 4:2:0 layout it has no macroblock structure for (the donor
//! panicked on it), and offsets, indices and plane copies use `get()`. Pixel loops index fixed
//! blocks bounded by that validated geometry. Tests: `tests.rs` (sweeps, fuzzing, absurd headers).

mod bits;
mod dct;
mod decode;
mod encode;
pub mod header;
mod spec_tables;
mod tables;

pub use decode::{DecodeOptions, decode_frame, decode_frame_with, probe};
pub use encode::{Encoder, EncoderConfig};
pub use header::{FieldCode, FrameHeader};
pub use tables::ri_frame_size;

/// Codec errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("truncated VC-3 data")]
    Truncated,
    #[error("invalid VC-3 data: {0}")]
    Invalid(&'static str),
    #[error("unknown VC-3 compression ID {0}")]
    UnknownCid(u32),
    #[error("corrupt macroblock scan line {0}")]
    Row(usize),
    #[error("invalid encoder input: {0}")]
    Input(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Chroma subsampling (`SSC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaFormat {
    /// Half width, half height.
    Yuv420,
    /// Half width, full height.
    Yuv422,
    /// Full size (also RGB).
    Yuv444,
}

impl ChromaFormat {
    pub fn chroma_width(self, width: u32) -> u32 {
        match self {
            ChromaFormat::Yuv444 => width,
            _ => width.div_ceil(2),
        }
    }
    pub fn chroma_height(self, height: u32) -> u32 {
        match self {
            ChromaFormat::Yuv420 => height.div_ceil(2),
            _ => height,
        }
    }
}

/// Colour volume (`CLV`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorVolume {
    #[default]
    Bt709,
    /// BT.2020 non-constant luminance.
    Bt2020Ncl,
    /// BT.2020 constant luminance.
    Bt2020Cl,
    /// Described out of band.
    OutOfBand,
}

/// Frame scan structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Interlace {
    #[default]
    Progressive,
    /// Interlaced; field 1 (the top field, even lines) comes first.
    TopFieldFirst,
}

/// DNxHR profiles (resolution-independent compression IDs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// DNxHR LB (CID 1274), 8-bit.
    Lb,
    /// DNxHR SQ (CID 1273), 8-bit.
    Sq,
    /// DNxHR HQ (CID 1272), 8-bit.
    Hq,
    /// DNxHR HQX (CID 1271), 10- or 12-bit.
    Hqx,
    /// DNxHR 444 (CID 1270), 10- or 12-bit 4:4:4.
    R444,
}

impl Profile {
    pub fn cid(self) -> u32 {
        match self {
            Profile::Lb => 1274,
            Profile::Sq => 1273,
            Profile::Hq => 1272,
            Profile::Hqx => 1271,
            Profile::R444 => 1270,
        }
    }
    pub fn from_cid(cid: u32) -> Option<Profile> {
        [Profile::Lb, Profile::Sq, Profile::Hq, Profile::Hqx, Profile::R444].into_iter().find(|p| p.cid() == cid)
    }
    pub fn name(self) -> &'static str {
        match self {
            Profile::Lb => "VC-3 LB",
            Profile::Sq => "VC-3 SQ",
            Profile::Hq => "VC-3 HQ",
            Profile::Hqx => "VC-3 HQX",
            Profile::R444 => "VC-3 444",
        }
    }

    /// The profile's short variant name ("LB", "SQ", "HQ", "HQX", "444").
    pub fn variant(self) -> &'static str {
        match self {
            Profile::Lb => "LB",
            Profile::Sq => "SQ",
            Profile::Hq => "HQ",
            Profile::Hqx => "HQX",
            Profile::R444 => "444",
        }
    }
    /// CBR frame size for a raster (equation 7.1).
    pub fn frame_size(self, width: u32, height: u32) -> u32 {
        let c0 = match self {
            Profile::Lb => 188416,
            Profile::Sq => 606208,
            Profile::Hq | Profile::Hqx => 917504,
            Profile::R444 => 1835008,
        };
        ri_frame_size(width, height, c0, false)
    }
}

/// The variant of a compression ID as on-screen labels name it: "LB", "SQ", "HQ", "HQX" or "444" for
/// the resolution-independent profiles; for a fixed-raster compression ID the picture's lines and
/// scan with its number ("1080p CID 1237", "1080i CID 1242").
pub fn cid_variant(cid: u32) -> String {
    if let Some(p) = Profile::from_cid(cid) {
        return p.variant().into();
    }
    match tables::cid_info(cid).map(|i| i.kind) {
        Some(tables::Kind::Hd { height, interlaced, .. }) => format!("{height}{} CID {cid}", if interlaced { 'i' } else { 'p' }),
        _ => format!("CID {cid}"),
    }
}

/// A human-readable name for a compression ID ("VC-3 HQX", "VC-3 1080i CID 1242").
pub fn cid_name(cid: u32) -> String {
    format!("VC-3 {}", cid_variant(cid))
}

/// The on-screen label of a stream: "VC-3 (HQX, 10-bit)". Names the standard and the variant only.
pub fn label(cid: u32, bit_depth: u8) -> String {
    format!("VC-3 ({}, {bit_depth}-bit)", cid_variant(cid))
}

/// A decoded (or to-be-encoded) picture: planar `u16` samples, rows packed without padding.
///
/// For RGB streams (`rgb == true`) the planes `y`, `cb`, `cr` hold G, B and R: the coded channel
/// order Ch1/Ch2/Ch3 as written by ffmpeg and read by its decoder (the standard's text names Ch1
/// red; see the README).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub chroma: ChromaFormat,
    pub rgb: bool,
    /// Significant bits per sample of every plane (8, 10 or 12).
    pub bit_depth: u8,
    pub y: Vec<u16>,
    pub cb: Vec<u16>,
    pub cr: Vec<u16>,
    /// Full-range alpha (0 = transparent).
    pub alpha: Option<Vec<u16>>,
    pub interlace: Interlace,
    pub color_volume: ColorVolume,
    /// Compression ID of the stream.
    pub cid: u32,
    /// Pixel aspect ratio (0/0 = not signalled).
    pub par: (u16, u16),
}

impl Frame {
    /// A black (video-range) frame. A raster of no pixels or more than the decoder accepts (the
    /// header limits, [`header::MAX_DIMENSION`] and the crate's picture-size limit) gives an empty
    /// 0 x 0 frame, so no caller can request a huge allocation; [`Frame::try_new`] reports it.
    pub fn new(width: u32, height: u32, chroma: ChromaFormat, bit_depth: u8, with_alpha: bool) -> Frame {
        Frame::try_new(width, height, chroma, bit_depth, with_alpha).unwrap_or_else(|_| Frame::build(0, 0, chroma, bit_depth.clamp(8, 16), false))
    }

    /// Like [`Frame::new`], but an out-of-range raster or bit depth is an error.
    pub fn try_new(width: u32, height: u32, chroma: ChromaFormat, bit_depth: u8, with_alpha: bool) -> Result<Frame> {
        if width == 0 || height == 0 || width > header::MAX_DIMENSION || u64::from(width) * u64::from(height) > crate::movie::MAX_PIXELS {
            return Err(Error::Invalid("raster size"));
        }
        if !(8..=16).contains(&bit_depth) {
            return Err(Error::Invalid("sample bit depth"));
        }
        Ok(Frame::build(width, height, chroma, bit_depth, with_alpha))
    }

    /// Allocate the planes of an already validated raster.
    fn build(width: u32, height: u32, chroma: ChromaFormat, bit_depth: u8, with_alpha: bool) -> Frame {
        let n = width as usize * height as usize;
        let cn = chroma.chroma_width(width) as usize * chroma.chroma_height(height) as usize;
        let s = bit_depth.saturating_sub(8) as u32;
        Frame {
            width,
            height,
            chroma,
            rgb: false,
            bit_depth,
            y: vec![(16u32 << s) as u16; n],
            cb: vec![(128u32 << s) as u16; cn],
            cr: vec![(128u32 << s) as u16; cn],
            alpha: with_alpha.then(|| vec![((1u32 << bit_depth) - 1) as u16; n]),
            interlace: Interlace::Progressive,
            color_volume: ColorVolume::Bt709,
            cid: 0,
            par: (0, 0),
        }
    }

    pub(crate) fn new_like(h: &FrameHeader, width: u32, height: u32) -> Result<Frame> {
        let mut f = Frame::try_new(width, height, h.chroma, h.bit_depth, h.alpha)?;
        f.rgb = h.rgb_planes();
        f.color_volume = h.color_volume;
        f.cid = h.cid;
        f.par = h.par;
        Ok(f)
    }

    pub fn chroma_width(&self) -> u32 {
        self.chroma.chroma_width(self.width)
    }

    pub fn chroma_height(&self) -> u32 {
        self.chroma.chroma_height(self.height)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod movie_tests;

#[cfg(test)]
mod fixture_tests;

#[cfg(test)]
mod oracle;
