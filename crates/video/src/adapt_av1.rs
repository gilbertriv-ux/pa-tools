//! AV1 as a [`GopDecoder`]. The decoder is the compile-time feature `av1-decode` (off by default);
//! without it every AV1 track opens as "unsupported in this build", so the timeline still shows the
//! clip and frame requests say why. With the feature on, the decoder lane replaces the two
//! `av1-decode` bodies below (and nothing else outside `av1/`).

use crate::gop::{Described, GopDecoder};
use crate::isobmff::Av1Config;
use crate::{Result, VideoError};

/// What the `av1C` record says about decodability (profile, bit depth, chroma format).
#[cfg(not(feature = "av1-decode"))]
pub(crate) fn describe(_cfg: &Av1Config) -> Described {
    Described::not_in_build("AV1")
}

/// A decoder primed with the sequence header OBUs from `av1C`.
#[cfg(not(feature = "av1-decode"))]
pub(crate) fn open(_config_obus: &[u8]) -> Result<Box<dyn GopDecoder>> {
    Err(VideoError::Unsupported("AV1 pictures are unsupported in this build".into()))
}

/// What the `av1C` record says about decodability (profile, bit depth, chroma format).
#[cfg(feature = "av1-decode")]
pub(crate) fn describe(_cfg: &Av1Config) -> Described {
    Described::not_yet("AV1")
}

/// A decoder primed with the sequence header OBUs from `av1C`.
#[cfg(feature = "av1-decode")]
pub(crate) fn open(_config_obus: &[u8]) -> Result<Box<dyn GopDecoder>> {
    Err(VideoError::Unsupported("AV1 pictures are not supported yet".into()))
}
