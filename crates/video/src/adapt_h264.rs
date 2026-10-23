//! H.264 as a [`GopDecoder`] (8-bit 4:2:0 Baseline / Main / High).

use crate::convert::{Planes, Yuv};
use crate::gop::{GopDecoder, Pic};
use crate::{Result, VideoError};

struct H264Gop(crate::h264::Decoder);

/// A decoder primed with the `avcC` record (parameter sets and NAL length size).
pub(crate) fn open(avcc: &[u8]) -> Result<Box<dyn GopDecoder>> {
    let dec = crate::h264::Decoder::from_avcc(avcc).map_err(|e| VideoError::Decode(e.to_string()))?;
    Ok(Box::new(H264Gop(dec)))
}

fn pic(p: crate::h264::Picture) -> Pic {
    let yuv = Yuv {
        width: p.width,
        height: p.height,
        planes: Planes::U8 { y: p.y, u: p.u, v: p.v },
        y_stride: p.y_stride,
        uv_stride: p.uv_stride,
        cx: 2,
        cy: 2,
        bit_depth: 8,
        matrix: p.color.matrix,
        full_range: p.color.full_range,
    };
    Pic { pts: p.pts, yuv }
}

impl GopDecoder for H264Gop {
    fn decode(&mut self, au: &[u8], pts: i64) -> Result<Vec<Pic>> {
        self.0.decode(au, pts).map(|v| v.into_iter().map(pic).collect()).map_err(|e| VideoError::Decode(e.to_string()))
    }

    fn flush(&mut self) -> Vec<Pic> {
        self.0.flush().into_iter().map(pic).collect()
    }
}
