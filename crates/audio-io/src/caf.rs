//! Size checks on CAF chunks before symphonia reads them: it allocates whatever a magic cookie,
//! a packet table or a packet claims, and a failed allocation aborts the process.

use crate::pcm::{PcmKind, be_u16, be_u32, be_u64};
use crate::stream::NativeLayout;
use crate::{AudioError, Result, SampleFormat};

/// Highest sample rate a `desc` chunk may claim (matches the AIFF reader's bound).
const MAX_SAMPLE_RATE: f64 = 10_000_000.0;

/// Parse the header (`desc` + `data`) of a Core Audio Format file for streamed reading, computing
/// the sample-data length from the real file length `file_len`. Returns `None` for any CAF whose
/// audio is not uncompressed linear PCM (handed to symphonia on a full decode). Reuses the same
/// big-endian helpers and chunk layout as [`check`].
pub(crate) fn stream_layout(b: &[u8], file_len: u64) -> Result<Option<NativeLayout>> {
    if b.get(0..4) != Some(b"caff") {
        return Err(AudioError::Malformed("not a CAF file".into()));
    }
    // mFileVersion(2) mFileFlags(2); chunks follow at offset 8.
    let mut pos = 8usize;
    let mut desc: Option<(f64, [u8; 4], u32, u32, u32, u32)> = None; // (rate, fmt_id, flags, frames/pkt, channels, bits)
    let mut data: Option<(u64, u64)> = None; // (data_start, declared_len against the file)
    let mut chunks = 0usize;
    while let (Some(kind), Some(size)) = (b.get(pos..pos + 4), be_u64(b, pos + 4)) {
        chunks += 1;
        if chunks > 100_000 {
            break;
        }
        let body = pos.checked_add(12).ok_or_else(|| AudioError::Malformed("CAF chunk offset overflow".into()))?;
        let size_i = size as i64;
        match kind {
            b"desc" if size == 32 => {
                let c = b.get(body..).unwrap_or_default();
                let short = || AudioError::Malformed("CAF desc chunk too short".into());
                let rate = f64::from_bits(be_u64(c, 0).ok_or_else(short)?);
                let fmt_id = c.get(8..12).and_then(|s| <[u8; 4]>::try_from(s).ok()).ok_or_else(short)?;
                let flags = be_u32(c, 12).ok_or_else(short)?;
                let frames_per_packet = be_u32(c, 20).ok_or_else(short)?;
                let channels = be_u32(c, 24).ok_or_else(short)?;
                let bits = be_u32(c, 28).ok_or_else(short)?;
                desc = Some((rate, fmt_id, flags, frames_per_packet, channels, bits));
            }
            b"data" if data.is_none() => {
                // The data body begins with a 4-byte mEditCount; audio follows it.
                let start = (body as u64).saturating_add(4).min(file_len);
                let avail = file_len.saturating_sub(start);
                let declared = if size_i < 0 { avail } else { size.saturating_sub(4).min(avail) };
                data = Some((start, declared));
            }
            _ => {}
        }
        if desc.is_some() && data.is_some() {
            break;
        }
        if size_i < 0 {
            break; // an unbounded `data` chunk must be the last chunk
        }
        pos = match body.checked_add(usize::try_from(size).unwrap_or(usize::MAX)) {
            Some(p) if p <= b.len() => p,
            _ => break,
        };
    }

    let (rate, fmt_id, flags, frames_per_packet, channels, bits) = desc.ok_or_else(|| AudioError::Malformed("CAF has no desc chunk".into()))?;
    if &fmt_id != b"lpcm" || frames_per_packet != 1 {
        return Ok(None); // a compressed or packetised codec: symphonia handles it
    }
    if !(1.0..=MAX_SAMPLE_RATE).contains(&rate) {
        return Err(AudioError::Malformed(format!("CAF sample rate {rate} out of range")));
    }
    if channels == 0 || channels > u32::from(u16::MAX) {
        return Err(AudioError::Malformed(format!("CAF channel count {channels} out of range")));
    }
    let is_float = flags & 0x1 != 0; // kCAFLinearPCMFormatFlagIsFloat
    let big_endian = flags & 0x2 == 0; // kCAFLinearPCMFormatFlagIsLittleEndian clear => big-endian
    let (kind, sample_format) = if is_float {
        match bits {
            32 => (PcmKind::Float { bytes: 4, big_endian }, SampleFormat::Float32),
            64 => (PcmKind::Float { bytes: 8, big_endian }, SampleFormat::Float64),
            other => return Err(AudioError::Unsupported(format!("CAF {other}-bit float"))),
        }
    } else {
        if !(1..=32).contains(&bits) {
            return Err(AudioError::Unsupported(format!("CAF {bits}-bit integer")));
        }
        let bytes = (bits as usize).div_ceil(8);
        let sf = match bits {
            1..=8 => SampleFormat::Int8,
            9..=16 => SampleFormat::Int16,
            17..=24 => SampleFormat::Int24,
            _ => SampleFormat::Int32,
        };
        (PcmKind::Int { bytes, big_endian, unsigned: false }, sf)
    };
    let (data_start, data_len) = data.ok_or_else(|| AudioError::Malformed("CAF has no data chunk".into()))?;
    Ok(Some(NativeLayout { sample_format, sample_rate: rate.round() as u32, channels: channels as u16, data_start, data_len, kind }))
}

/// Refuse the CAF stream at `at` if its magic cookie, packet table or packets claim more bytes
/// than the file holds.
pub(crate) fn check(b: &[u8], at: usize) -> Result<()> {
    if be_u16(b, at + 4) == Some(1) { chunks(b, at + 8) } else { Ok(()) }
}

fn malformed(what: &str) -> AudioError {
    AudioError::Malformed(format!("CAF {what} is larger than the file"))
}

/// Walk the chunks the way symphonia 0.5 reads them: it advances by the bytes it parses, not by
/// the declared size, and after a `data` chunk of unknown size it reads on into the audio.
fn chunks(b: &[u8], mut pos: usize) -> Result<()> {
    // Whether the packet table lists each packet's size and frame count (variable per packet).
    let (mut lists_sizes, mut lists_frames, mut described) = (false, false, false);
    while let (Some(kind), Some(size)) = (b.get(pos..pos + 4), be_u64(b, pos + 4)) {
        let (body, size) = (pos + 12, size as i64);
        let used = match kind {
            b"desc" if size == 32 => {
                lists_sizes = be_u32(b, body + 16) == Some(0);
                lists_frames = be_u32(b, body + 20) == Some(0);
                described = true;
                Some(32)
            }
            b"data" if size == -1 => Some(4),
            b"chan" if size >= 12 => be_u32(b, body + 8).map(|n| 12 + u64::from(n) * 20),
            b"pakt" if size >= 24 && described => packet_table(b, body, lists_sizes, lists_frames)?,
            b"kuki" if size >= 0 && size as u64 > b.len().saturating_sub(body) as u64 => return Err(malformed("magic cookie")),
            b"desc" | b"pakt" | b"chan" => None,
            _ => u64::try_from(size).ok(),
        };
        match used.filter(|_| described).and_then(|n| usize::try_from(n).ok()).and_then(|n| body.checked_add(n)) {
            Some(next) if next < b.len() => pos = next,
            _ => return Ok(()),
        }
    }
    Ok(())
}

/// Check a packet table's count and packet sizes; returns the bytes symphonia reads for it, or
/// None where symphonia stops. It reserves room for every packet before reading any, so a table
/// it would refuse after that, or that runs past the file, is refused first.
fn packet_table(b: &[u8], body: usize, lists_sizes: bool, lists_frames: bool) -> Result<Option<u64>> {
    let valid = |at: usize| be_u64(b, at).filter(|&n| n as i64 >= 0);
    let (Some(packets), Some(_)) = (valid(body), valid(body + 8)) else { return Ok(None) };
    let mut at = body + 24;
    if packets > b.len().saturating_sub(at) as u64 {
        return Err(malformed("packet table"));
    }
    if !lists_sizes && !lists_frames {
        return if packets == 0 { Ok(Some(24)) } else { Err(AudioError::Malformed("a constant bit rate CAF lists packets".into())) };
    }
    for _ in 0..packets {
        if lists_sizes {
            let Some((size, len)) = varint(b, at) else { return Err(malformed("packet table")) };
            if size > b.len() as u64 {
                return Err(malformed("packet"));
            }
            at += len;
        }
        if lists_frames {
            let Some((_, len)) = varint(b, at) else { return Err(malformed("packet table")) };
            at += len;
        }
    }
    Ok(Some((at - body) as u64))
}

/// A CAF variable-length integer at `at` and its length in bytes.
fn varint(b: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..9 {
        let x = *b.get(at + i)?;
        v |= u64::from(x & 0x7f);
        if x & 0x80 == 0 {
            return Some((v, i + 1));
        }
        v <<= 7;
    }
    None
}
