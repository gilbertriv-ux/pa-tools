//! VC-3 (SMPTE ST 2019-1) as an intra-only codec: the fixed-raster HD compression IDs and the
//! resolution-independent profiles (LB, SQ, HQ, HQX, 444), 8, 10 and 12 bits, 4:2:0 / 4:2:2 /
//! 4:4:4, Y'CbCr or R'G'B', progressive or interlaced. Always compiled in. In a MOV the sample
//! entry is `AVdn` (fixed rasters) or `AVdh` (resolution-independent).
//!
//! The on-screen label is "VC-3" plus the variant and bit depth, e.g. "VC-3 (SQ, 8-bit)", read from
//! the first sample's header. A track whose first header cannot be read is still labelled "VC-3"
//! and offered for decoding (damaged samples fail one at a time); only a compression ID outside
//! the standard is reported as unsupported.
//!
//! Alpha, where a stream codes it, is not used: pictures are shown opaque, like the other intra
//! codecs. Colour is converted with the stream's own matrix (BT.709, or BT.2020 non-constant
//! luminance; "out of band" falls back to the picture-height rule) and video range; R'G'B' streams
//! convert directly. Nothing is tone-mapped.

use crate::MovieInfo;
use crate::convert::{Matrix, planar16_to_rgba, rgb16_to_rgba};
use crate::gop::{Described, RgbaPicture};
use crate::movie::MAX_PIXELS;
use crate::vc3::{self, ChromaFormat, ColorVolume, DecodeOptions, Error};
use crate::{Result, VideoError};

/// What the track's sample entry and its first sample's header (`first_sample`, up to the first
/// kilobyte, when it could be read) say about decodability.
pub(crate) fn describe(_fourcc: [u8; 4], first_sample: Option<&[u8]>) -> Described {
    match first_sample.map(vc3::header::parse_prefix) {
        Some(Ok(h)) => Described { detail: vc3::label(h.cid, h.bit_depth), unsupported: None },
        Some(Err(Error::UnknownCid(cid))) => {
            Described { detail: "VC-3".into(), unsupported: Some(format!("VC-3 compression ID {cid} is not supported")) }
        }
        // Unreadable or damaged first header: say what the track claims to be and let each picture
        // report its own damage.
        _ => Described { detail: "VC-3".into(), unsupported: None },
    }
}

fn decode_error(e: Error) -> VideoError {
    match e {
        Error::UnknownCid(cid) => VideoError::Unsupported(format!("VC-3 compression ID {cid} is not supported")),
        e => VideoError::Decode(e.to_string()),
    }
}

/// Decode one frame to RGBA. `track` is the movie's declared picture format: a frame whose header
/// claims another size is refused.
pub(crate) fn decode(sample: &[u8], track: &MovieInfo) -> Result<RgbaPicture> {
    let hdr = vc3::probe(sample).map_err(decode_error)?;
    // The planes are sized from the header: refuse a picture the track does not describe (or one no
    // movie may have) before allocating anything.
    let (w, h) = (hdr.width, hdr.frame_lines());
    if (w, h) != (track.width, track.height) {
        return Err(VideoError::Decode(format!("{w}x{h} frame in a {}x{} track", track.width, track.height)));
    }
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(VideoError::Decode("picture too large".into()));
    }
    let f = vc3::decode_frame_with(sample, &DecodeOptions { threads: false, spec_dequant: false }).map_err(decode_error)?;
    let (width, height) = (f.width as usize, f.height as usize);
    let rgba = if f.rgb {
        // The planes hold G, B, R in video range.
        rgb16_to_rgba(width, height, &f.cr, &f.y, &f.cb, width, f.bit_depth, false)
    } else {
        // Constant-luminance BT.2020 is shown with the non-constant matrix (the nearest we have).
        let code = match f.color_volume {
            ColorVolume::Bt709 => 1,
            ColorVolume::Bt2020Ncl | ColorVolume::Bt2020Cl => 9,
            ColorVolume::OutOfBand => 0,
        };
        let m = Matrix::for_code(code, f.height, false);
        let (cx, cy) = match f.chroma {
            ChromaFormat::Yuv420 => (2, 2),
            ChromaFormat::Yuv422 => (2, 1),
            ChromaFormat::Yuv444 => (1, 1),
        };
        planar16_to_rgba(width, height, &f.y, width, &f.cb, &f.cr, f.chroma_width() as usize, cx, cy, f.bit_depth, &m)
    };
    if rgba.len() as u64 != u64::from(f.width) * u64::from(f.height) * 4 {
        return Err(VideoError::Decode("picture too large".into()));
    }
    Ok(RgbaPicture { width: f.width, height: f.height, rgba })
}
