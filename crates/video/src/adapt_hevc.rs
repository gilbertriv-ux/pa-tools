//! HEVC (H.265) as a [`GopDecoder`]. The decoder is the compile-time feature `hevc-decode` (off by
//! default); without it every HEVC track opens as "unsupported in this build", so the timeline
//! still shows the clip and frame requests say why. With the feature on, Main and Main 10 (4:2:0,
//! 8 to 12 bits) decode; 4:0:0, 4:2:2, 4:4:4 and the range-extension tools open the same way, with
//! the reason on the track.

use crate::gop::{Described, GopDecoder};
use crate::isobmff::HevcConfig;
use crate::{Result, VideoError};

/// The on-screen name of an `hvcC` profile ("HEVC" when the record names none we know).
fn label(cfg: &HevcConfig) -> &'static str {
    match cfg.general_profile_idc {
        1 => "H.265 Main",
        2 => "H.265 Main 10",
        3 => "H.265 Main Still Picture",
        4 => "H.265 Range Extensions",
        5 => "H.265 High Throughput",
        6 => "H.265 Multiview Main",
        7 => "H.265 Scalable Main",
        8 => "H.265 3D Main",
        9 => "H.265 Screen Content",
        _ => "HEVC",
    }
}

/// What the `hvcC` record says about decodability (profile, chroma format, bit depth).
#[cfg(not(feature = "hevc-decode"))]
pub(crate) fn describe(cfg: &HevcConfig) -> Described {
    Described::not_in_build(label(cfg))
}

/// A decoder primed with the `hvcC` record.
#[cfg(not(feature = "hevc-decode"))]
pub(crate) fn open(_hvcc: &[u8]) -> Result<Box<dyn GopDecoder>> {
    Err(VideoError::Unsupported("HEVC pictures are unsupported in this build".into()))
}

/// What the `hvcC` record (and the sequence parameter set in it, when it holds one) says about
/// decodability: profile, chroma format, bit depth, picture size.
#[cfg(feature = "hevc-decode")]
pub(crate) fn describe(cfg: &HevcConfig) -> Described {
    let detail = label(cfg);
    // The sequence parameter set is the authority; a record without one (or with one that does
    // not parse) is judged by its own fields.
    let sps = cfg
        .arrays
        .iter()
        .filter(|a| a.nal_type == crate::hevc::slice::nal_type::SPS)
        .flat_map(|a| a.nalus.iter())
        .find_map(|n| n.get(2..).and_then(|rbsp| crate::hevc::params::Sps::parse(&crate::bitstream::unescape_rbsp(rbsp)).ok()));
    let (chroma, luma_depth, chroma_depth) = match &sps {
        Some(s) => (s.chroma_format_idc, s.bit_depth_luma, s.bit_depth_chroma),
        None => (u32::from(cfg.chroma_format_idc), u32::from(cfg.bit_depth_luma), u32::from(cfg.bit_depth_chroma)),
    };
    let unsupported = if chroma != 1 {
        let what = match chroma {
            0 => "4:0:0 (monochrome)",
            2 => "4:2:2",
            _ => "4:4:4",
        };
        Some(format!("{detail} {what} pictures are not supported yet"))
    } else if luma_depth > 12 || chroma_depth > 12 {
        Some(format!("{detail} {}-bit pictures are not supported yet", luma_depth.max(chroma_depth)))
    } else if luma_depth != chroma_depth {
        Some(format!("{detail} with different luma and chroma bit depths ({luma_depth}/{chroma_depth}) is not supported yet"))
    } else {
        sps.as_ref().and_then(|s| match s.check_supported() {
            Err(crate::hevc::Error::Unsupported(why)) => Some(format!("{detail}: {why}")),
            _ => None,
        })
    };
    Described { detail: detail.to_string(), unsupported }
}

/// A decoder primed with the `hvcC` record.
#[cfg(feature = "hevc-decode")]
pub(crate) fn open(hvcc: &[u8]) -> Result<Box<dyn GopDecoder>> {
    // One thread: the movie decodes on its own background thread, and a pool per thumbnail decoder
    // would cost more than it saves.
    let mut dec = crate::hevc::Decoder::with_threads(1);
    dec.configure_hvcc(hvcc).map_err(error)?;
    Ok(Box::new(HevcGop(dec)))
}

#[cfg(feature = "hevc-decode")]
struct HevcGop(crate::hevc::Decoder);

/// A stream that is valid but outside the decoder stops the movie's decode; anything else is a
/// damaged access unit, which the movie skips.
#[cfg(feature = "hevc-decode")]
fn error(e: crate::hevc::Error) -> VideoError {
    match e {
        crate::hevc::Error::Unsupported(why) => VideoError::Unsupported(format!("HEVC: {why}")),
        other => VideoError::Decode(other.to_string()),
    }
}

#[cfg(feature = "hevc-decode")]
fn pic(p: crate::hevc::Picture) -> Result<crate::gop::Pic> {
    use crate::convert::{Planes, Yuv};
    use crate::hevc::Plane;
    let (planes, bit_depth) = match (p.y, p.u, p.v) {
        (Plane::U8(y), Plane::U8(u), Plane::U8(v)) => (Planes::U8 { y, u, v }, 8),
        (Plane::U16(y), Plane::U16(u), Plane::U16(v)) => (Planes::U16 { y, u, v }, u8::try_from(p.bit_depth).unwrap_or(16)),
        _ => return Err(VideoError::Decode("HEVC planes of different sample types".into())),
    };
    let yuv = Yuv {
        width: p.width,
        height: p.height,
        planes,
        y_stride: p.y_stride,
        uv_stride: p.uv_stride,
        cx: 2,
        cy: 2,
        bit_depth,
        matrix: p.color.matrix,
        full_range: p.color.full_range,
    };
    Ok(crate::gop::Pic { pts: p.pts, yuv })
}

#[cfg(feature = "hevc-decode")]
impl GopDecoder for HevcGop {
    fn decode(&mut self, au: &[u8], pts: i64) -> Result<Vec<crate::gop::Pic>> {
        self.0.decode(au, pts).map_err(error)?.into_iter().map(pic).collect()
    }

    fn flush(&mut self) -> Vec<crate::gop::Pic> {
        self.0.flush().into_iter().filter_map(|p| pic(p).ok()).collect()
    }

    /// Containers flag sync samples loosely; HEVC says exactly which pictures start a stream.
    fn checks_random_access(&self) -> bool {
        true
    }

    fn is_random_access(&self, au: &[u8]) -> bool {
        self.0.is_random_access(au)
    }
}
