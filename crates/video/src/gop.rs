//! The seam between [`crate::Movie`] and its picture codecs.
//!
//! A long-GOP codec (H.264 today; HEVC and AV1 plug in the same way) is a [`GopDecoder`]: the movie
//! feeds it whole container samples forward from a random-access sample and collects the pictures
//! it outputs, tagged with the presentation index the movie gave each access unit. Everything else
//! about a codec (what its sample entry means, which variants it can decode, how it is built) lives
//! in one `adapt_<codec>.rs` file that [`crate::Movie`] calls at open and when it needs a decoder,
//! so adding a codec touches no code in `movie.rs`.
//!
//! Intra-only codecs (ProRes, Motion JPEG, DNxHD/DNxHR) need no decoder state and are decoded one
//! sample at a time; their adapters return packed RGBA ([`RgbaPicture`]).

use crate::Result;
use crate::convert::Yuv;

/// One decoded picture handed back by a [`GopDecoder`].
pub(crate) struct Pic {
    /// The presentation timestamp passed to [`GopDecoder::decode`] with this picture's access unit
    /// ([`crate::Movie`] passes the picture's presentation index).
    pub pts: i64,
    pub yuv: Yuv,
}

/// A decoder for a codec with inter-picture prediction and picture reordering.
///
/// Errors are values: a decoder never panics on hostile data. Return [`crate::VideoError::Unsupported`]
/// for a stream that is valid but outside what the decoder implements (the movie stops at once),
/// and [`crate::VideoError::Decode`] for a damaged access unit (the movie skips it and carries on).
pub(crate) trait GopDecoder: Send {
    /// Feed one access unit (one container sample). Returns the pictures that became ready for
    /// output, in output order; `pts` travels with this access unit's picture through reordering.
    fn decode(&mut self, au: &[u8], pts: i64) -> Result<Vec<Pic>>;

    /// Output everything still held back (end of stream).
    fn flush(&mut self) -> Vec<Pic>;

    /// True if the container's sync flag cannot be trusted for this codec, so the movie asks
    /// [`GopDecoder::is_random_access`] before starting a decode at a sync sample.
    fn checks_random_access(&self) -> bool {
        false
    }

    /// True if decoding may start at this access unit (only asked when
    /// [`GopDecoder::checks_random_access`] is true). When it is false the movie steps back to the
    /// previous sync sample, a bounded number of times.
    fn is_random_access(&self, _au: &[u8]) -> bool {
        true
    }
}

/// A packed RGBA picture from an intra-only codec's adapter.
pub(crate) struct RgbaPicture {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// What an adapter reports about a track when the movie is opened.
pub(crate) struct Described {
    /// Human-readable codec description for [`crate::MovieInfo::codec_detail`].
    pub detail: String,
    /// `Some(why)` when this stream cannot be decoded, so the movie opens but frame requests
    /// return [`crate::VideoError::Unsupported`] with `why`.
    pub unsupported: Option<String>,
}

impl Described {
    /// The codec is recognised (`label` is its on-screen name), but its decoder is not written yet.
    pub fn not_yet(label: &str) -> Described {
        Described { detail: label.to_string(), unsupported: Some(format!("{label} pictures are not supported yet")) }
    }

    /// The codec has a decoder, but it is a compile-time feature that this build leaves out.
    #[cfg_attr(all(feature = "hevc-decode", feature = "av1-decode"), allow(dead_code))]
    pub fn not_in_build(label: &str) -> Described {
        Described { detail: label.to_string(), unsupported: Some(format!("{label} pictures are unsupported in this build")) }
    }
}
