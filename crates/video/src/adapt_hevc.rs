//! HEVC as a [`GopDecoder`]. The decoder is the compile-time feature `hevc-decode` (off by default);
//! without it every HEVC track opens as "unsupported in this build", so the timeline still shows the
//! clip and frame requests say why. With the feature on, the decoder lane replaces the two
//! `hevc-decode` bodies below (and nothing else outside `hevc/`).

use crate::gop::{Described, GopDecoder};
use crate::isobmff::HevcConfig;
use crate::{Result, VideoError};

/// What the `hvcC` record says about decodability (profile, chroma format, bit depth).
#[cfg(not(feature = "hevc-decode"))]
pub(crate) fn describe(_cfg: &HevcConfig) -> Described {
    Described::not_in_build("HEVC")
}

/// A decoder primed with the `hvcC` record.
#[cfg(not(feature = "hevc-decode"))]
pub(crate) fn open(_hvcc: &[u8]) -> Result<Box<dyn GopDecoder>> {
    Err(VideoError::Unsupported("HEVC pictures are unsupported in this build".into()))
}

/// What the `hvcC` record says about decodability (profile, chroma format, bit depth).
#[cfg(feature = "hevc-decode")]
pub(crate) fn describe(_cfg: &HevcConfig) -> Described {
    Described::not_yet("HEVC")
}

/// A decoder primed with the `hvcC` record.
#[cfg(feature = "hevc-decode")]
pub(crate) fn open(_hvcc: &[u8]) -> Result<Box<dyn GopDecoder>> {
    Err(VideoError::Unsupported("HEVC pictures are not supported yet".into()))
}
