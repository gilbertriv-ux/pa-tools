//! Minimal WAVE (RIFF) and AIFF containers for audio essence embedded in AAF / OMF documents.
//! PCM inside the model is interleaved little-endian signed integers (8-bit unsigned for WAVE).

use crate::bytes::{be16, be32, le16, le32, slice};

/// Bit depths the containers here carry.
fn bits_ok(bits: u16) -> bool {
    matches!(bits, 8 | 16 | 24 | 32)
}

/// `n` as a `u32` field, saturating (a size field that cannot hold a huge source says so
/// by being full, instead of wrapping).
fn field(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A complete WAVE file around little-endian PCM.
pub fn wav_file(pcm: &[u8], channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let mut v = wav_header(field(pcm.len()), channels, sample_rate, bits);
    v.extend_from_slice(pcm);
    if pcm.len() % 2 == 1 {
        v.push(0);
    }
    v
}

/// The RIFF header of a WAVE file up to and including the `data` chunk header.
pub fn wav_header(data_len: u32, channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let block = channels.max(1).saturating_mul(bits.div_ceil(8));
    let mut v = Vec::with_capacity(44);
    v.extend_from_slice(b"RIFF");
    // Saturating: a huge source (data_len saturated by the caller) must not overflow.
    v.extend_from_slice(&36u32.saturating_add(data_len).saturating_add(data_len & 1).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.max(1).to_le_bytes());
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&sample_rate.saturating_mul(u32::from(block)).to_le_bytes());
    v.extend_from_slice(&block.to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v
}

/// The chunks of a RIFF / IFF body as (id, body) where the body is cut at the end of the file (a
/// chunk whose declared size runs past it is truncated, as writers that were interrupted leave
/// them). Iteration ends at the first header that does not fit.
fn chunks(b: &[u8], start: usize, be: bool) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut at = Some(start);
    std::iter::from_fn(move || {
        let pos = at?;
        let id = slice(b, pos, 4)?;
        let len = usize::try_from(if be { be32(b, pos.checked_add(4)?)? } else { le32(b, pos.checked_add(4)?)? }).ok()?;
        let body_at = pos.checked_add(8)?;
        let body = b.get(body_at..body_at.saturating_add(len).min(b.len()))?;
        // chunks are padded to an even length
        at = body_at.checked_add(len).and_then(|n| n.checked_add(len & 1));
        Some((id, body))
    })
}

/// PCM, channels, sample rate and bits of a WAVE file (integer PCM only).
pub fn parse_wav(b: &[u8]) -> Option<(Vec<u8>, u16, u32, u16)> {
    if slice(b, 0, 4)? != b"RIFF" || slice(b, 8, 4)? != b"WAVE" {
        return None;
    }
    let mut fmt = None;
    for (id, body) in chunks(b, 12, false) {
        if id == b"fmt " && body.len() >= 16 {
            let tag = le16(body, 0)?;
            let ch = le16(body, 2)?;
            let sr = le32(body, 4)?;
            let bits = le16(body, 14)?;
            // an extensible format names its real format (PCM is 1) in the sub-format id
            let integer = match tag {
                1 => true,
                0xFFFE => le16(body, 24).is_none_or(|sub| sub == 1),
                _ => false,
            };
            if !integer || ch == 0 || !bits_ok(bits) {
                return None;
            }
            fmt = Some((ch, sr, bits));
        } else if id == b"data" {
            let (ch, sr, bits) = fmt?;
            return Some((body.to_vec(), ch, sr, bits));
        }
    }
    None
}

/// A complete AIFF file (big-endian PCM) from little-endian PCM (tests build files with it).
#[cfg(test)]
pub fn aiff_file(pcm: &[u8], channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let bps = bits.div_ceil(8) as usize;
    let frames = pcm.len() / (bps * channels.max(1) as usize).max(1);
    let mut v = aiff_header(frames as u32, channels, sample_rate, bits);
    for s in pcm.chunks_exact(bps.max(1)) {
        if bps == 1 {
            v.push(s[0] ^ 0x80); // WAVE 8-bit is unsigned, AIFF signed
        } else {
            v.extend(s.iter().rev());
        }
    }
    if (v.len() - 8) % 2 == 1 {
        v.push(0);
    }
    v
}

/// The AIFF header up to the sound data (`FORM`, `COMM`, `SSND` with offset and block size).
pub fn aiff_header(frames: u32, channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    // u64 so that no product overflows; the size fields then saturate
    let data_len = u64::from(frames) * u64::from(channels.max(1)) * u64::from(bits.div_ceil(8));
    let size = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    let mut v = Vec::with_capacity(54);
    v.extend_from_slice(b"FORM");
    v.extend_from_slice(&size(4 + 26 + 16 + data_len + (data_len & 1)).to_be_bytes());
    v.extend_from_slice(b"AIFFCOMM");
    v.extend_from_slice(&18u32.to_be_bytes());
    v.extend_from_slice(&channels.max(1).to_be_bytes());
    v.extend_from_slice(&frames.to_be_bytes());
    v.extend_from_slice(&bits.to_be_bytes());
    v.extend_from_slice(&extended(f64::from(sample_rate)));
    v.extend_from_slice(b"SSND");
    v.extend_from_slice(&size(8 + data_len).to_be_bytes());
    v.extend_from_slice(&[0; 8]);
    v
}

/// PCM (converted to little-endian), channels, sample rate and bits of an AIFF / AIFF-C (`NONE`
/// / `twos` / `sowt`) file.
pub fn parse_aiff(b: &[u8]) -> Option<(Vec<u8>, u16, u32, u16)> {
    if slice(b, 0, 4)? != b"FORM" || !matches!(slice(b, 8, 4)?, b"AIFF" | b"AIFC") {
        return None;
    }
    let mut comm = None;
    for (id, body) in chunks(b, 12, true) {
        if id == b"COMM" && body.len() >= 18 {
            let ch = be16(body, 0)?;
            let bits = be16(body, 6)?;
            if ch == 0 || !bits_ok(bits) {
                return None;
            }
            let sr = from_extended(crate::bytes::array::<10>(body, 8)?).round() as u32;
            let little = slice(body, 18, 4).is_some_and(|c| c == b"sowt");
            comm = Some((ch, sr, bits, little));
        } else if id == b"SSND" && body.len() >= 8 {
            let (ch, sr, bits, little) = comm?;
            let off = usize::try_from(be32(body, 0)?).ok()?;
            let data = body.get(off.checked_add(8)?..)?;
            let bps = usize::from(bits.div_ceil(8)).max(1);
            let mut pcm = Vec::with_capacity(data.len());
            for s in data.chunks_exact(bps) {
                if bps == 1 {
                    pcm.extend(s.iter().map(|x| x ^ 0x80));
                } else if little {
                    pcm.extend_from_slice(s);
                } else {
                    pcm.extend(s.iter().rev());
                }
            }
            return Some((pcm, ch, sr, bits));
        }
    }
    None
}

/// 80-bit IEEE 754 extended (AIFF sample rates).
fn extended(x: f64) -> [u8; 10] {
    if x <= 0.0 || !x.is_finite() {
        return [0u8; 10];
    }
    let e = x.log2().floor().clamp(-16383.0, 16383.0) as i32;
    let m = (x / 2f64.powi(e) * (1u64 << 63) as f64) as u64;
    let [e0, e1] = u16::try_from(e + 16383).unwrap_or(0).to_be_bytes();
    let [m0, m1, m2, m3, m4, m5, m6, m7] = m.to_be_bytes();
    [e0, e1, m0, m1, m2, m3, m4, m5, m6, m7]
}

fn from_extended(b: [u8; 10]) -> f64 {
    let [e0, e1, m0, m1, m2, m3, m4, m5, m6, m7] = b;
    let e = i32::from(u16::from_be_bytes([e0, e1]) & 0x7FFF);
    let m = u64::from_be_bytes([m0, m1, m2, m3, m4, m5, m6, m7]);
    if e == 0 && m == 0 {
        return 0.0;
    }
    m as f64 / (1u64 << 63) as f64 * 2f64.powi(e - 16383)
}

#[cfg(test)]
mod tests {
    /// A mutated EDL whose clip ran for days produced an OMF descriptor for more than 4 GiB of
    /// PCM: the RIFF size field overflowed ("attempt to add with overflow").
    #[test]
    fn huge_wav_header_saturates() {
        let h = super::wav_header(u32::MAX, 2, u32::MAX, 24);
        assert_eq!(&h[4..8], &u32::MAX.to_le_bytes());
    }

    use super::*;

    #[test]
    fn wav_and_aiff_round_trip() {
        let pcm: Vec<u8> = (0..48u8).collect(); // 8 stereo 24-bit frames
        let w = wav_file(&pcm, 2, 48_000, 24);
        assert_eq!(parse_wav(&w), Some((pcm.clone(), 2, 48_000, 24)));
        let a = aiff_file(&pcm, 2, 44_100, 24);
        assert_eq!(parse_aiff(&a), Some((pcm.clone(), 2, 44_100, 24)));
        let p16: Vec<u8> = (0..32u8).collect();
        assert_eq!(parse_aiff(&aiff_file(&p16, 1, 96_000, 16)), Some((p16, 1, 96_000, 16)));
        assert!(parse_wav(b"RIFF").is_none());
        assert!(parse_aiff(&a[..30]).is_none());
        for cut in 0..w.len() {
            let _ = parse_wav(&w[..cut]);
            let _ = parse_aiff(&a[..cut.min(a.len())]);
        }
    }
}
