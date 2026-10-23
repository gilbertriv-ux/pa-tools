//! VC-3 (SMPTE ST 2019, DNxHD / DNxHR) as an intra-only codec. Always compiled in. Not implemented
//! yet: the stub reports every such track as unsupported, so files open (the timeline shows the
//! clip) and frame requests say why. The VC-3 decoder lane replaces the bodies below and nothing
//! else outside `dnx/`. Its on-screen label is "VC-3" plus the variant, e.g. "VC-3 (SQ, 10-bit)".

use crate::MovieInfo;
use crate::gop::{Described, RgbaPicture};
use crate::{Result, VideoError};

/// What the track's sample entry and its first sample's header (`first_sample`, up to the first
/// kilobyte, when it could be read) say about decodability.
pub(crate) fn describe(_fourcc: [u8; 4], _first_sample: Option<&[u8]>) -> Described {
    Described::not_yet("VC-3")
}

/// Decode one frame to RGBA. `track` is the movie's declared picture format: a frame whose header
/// claims another size is refused.
pub(crate) fn decode(_sample: &[u8], _track: &MovieInfo) -> Result<RgbaPicture> {
    Err(VideoError::Unsupported("VC-3 pictures are not supported yet".into()))
}
