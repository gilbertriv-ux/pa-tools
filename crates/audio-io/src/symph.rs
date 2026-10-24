//! Decode path for everything that isn't native PCM WAV/AIFF, backed by `symphonia`.

use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{
    CODEC_TYPE_AAC, CODEC_TYPE_ADPCM_G722, CODEC_TYPE_ADPCM_G726, CODEC_TYPE_ADPCM_G726LE, CODEC_TYPE_ADPCM_IMA_QT, CODEC_TYPE_ADPCM_IMA_WAV,
    CODEC_TYPE_ADPCM_MS, CODEC_TYPE_ALAC, CODEC_TYPE_FLAC, CODEC_TYPE_MP1, CODEC_TYPE_MP2, CODEC_TYPE_MP3, CODEC_TYPE_NULL, CODEC_TYPE_OPUS,
    CODEC_TYPE_PCM_ALAW, CODEC_TYPE_PCM_F32BE, CODEC_TYPE_PCM_F32LE, CODEC_TYPE_PCM_F64BE, CODEC_TYPE_PCM_F64LE, CODEC_TYPE_PCM_MULAW,
    CODEC_TYPE_VORBIS, CodecParameters, CodecType, DecoderOptions,
};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions, ReadBytes};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Instantiate;

use crate::pcm::{MAX_DECODE_SAMPLES, be_u32};
use crate::{AudioBuffer, AudioError, AudioInfo, FileFormat, Result, SampleFormat};

/// The most frames a packet may declare: decoders allocate a packet's frames up front, and ALAC
/// takes its frame length from the file (encoders use 4096).
const MAX_PACKET_FRAMES: u64 = 1 << 20;

fn map_err(e: SymError) -> AudioError {
    match e {
        SymError::Unsupported(m) => AudioError::Unsupported(m.to_string()),
        SymError::LimitError(m) => AudioError::TooLarge(m.to_string()),
        other => AudioError::Malformed(other.to_string()),
    }
}

/// Run `f`, turning a panic inside the third-party decoder into an error.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(AudioError::Malformed("decoder failed on malformed input".into())))
}

/// What [`open`] found: a reader, or a stream that holds no audio frames and has no reader.
enum Opened {
    Reader(Box<dyn FormatReader>),
    Empty(AudioInfo),
}

/// Symphonia's probe loop (`Probe::format`, whose hint 0.5 ignores), checking the container it
/// finds before its reader allocates whatever the file claims.
fn open(bytes: &[u8]) -> Result<Opened> {
    let mut mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), MediaSourceStreamOptions::default());
    let fmt_opts = FormatOptions { enable_gapless: true, ..FormatOptions::default() };
    loop {
        let found = symphonia::default::get_probe().next(&mut mss).map_err(map_err)?;
        let at = usize::try_from(mss.pos()).unwrap_or(usize::MAX);
        let head = bytes.get(at..).unwrap_or_default();
        match (found, head.get(..10)) {
            (Instantiate::Format(reader), _) => {
                match head.get(..4) {
                    Some(b"caff") => crate::caf::check(bytes, at)?,
                    Some(b"ftyp") => crate::mp4::check(bytes, at.saturating_sub(4))?,
                    Some(b"fLaC") => {
                        crate::flac::check_metadata(bytes, at)?;
                        if let Some(info) = crate::flac::empty_stream_info(bytes, at) {
                            return Ok(Opened::Empty(info));
                        }
                    }
                    Some(b"RIFF") => crate::wav::check_info(bytes, at)?,
                    _ => {}
                }
                return reader(mss, &fmt_opts).map(Opened::Reader).map_err(map_err);
            }
            // Tags aren't used, and symphonia allocates whatever an ID3v2 frame claims: skip it.
            (Instantiate::Metadata(_), Some([b'I', b'D', b'3', _, _, _, size @ ..])) => {
                let size = size.iter().fold(0u64, |v, &x| (v << 7) | u64::from(x & 0x7f));
                mss.ignore_bytes(10 + size).map_err(|e| AudioError::Malformed(e.to_string()))?;
            }
            (Instantiate::Metadata(tag), _) => {
                tag(&MetadataOptions::default()).read_all(&mut mss).map_err(map_err)?;
            }
        }
    }
}

fn format_from_codec(codec: CodecType, detected: FileFormat) -> FileFormat {
    if detected != FileFormat::Other {
        return detected;
    }
    match codec {
        c if c == CODEC_TYPE_FLAC => FileFormat::Flac,
        c if c == CODEC_TYPE_MP3 || c == CODEC_TYPE_MP2 || c == CODEC_TYPE_MP1 => FileFormat::Mp3,
        c if c == CODEC_TYPE_AAC => FileFormat::Aac,
        c if c == CODEC_TYPE_ALAC => FileFormat::Alac,
        c if c == CODEC_TYPE_VORBIS => FileFormat::Ogg,
        _ => FileFormat::Other,
    }
}

fn sample_format_of(p: &CodecParameters) -> SampleFormat {
    let c = p.codec;
    if c == CODEC_TYPE_PCM_F32LE || c == CODEC_TYPE_PCM_F32BE {
        return SampleFormat::Float32;
    }
    if c == CODEC_TYPE_PCM_F64LE || c == CODEC_TYPE_PCM_F64BE {
        return SampleFormat::Float64;
    }
    let lossy = [
        CODEC_TYPE_MP1,
        CODEC_TYPE_MP2,
        CODEC_TYPE_MP3,
        CODEC_TYPE_AAC,
        CODEC_TYPE_VORBIS,
        CODEC_TYPE_OPUS,
        CODEC_TYPE_ADPCM_MS,
        CODEC_TYPE_ADPCM_IMA_WAV,
        CODEC_TYPE_ADPCM_IMA_QT,
        CODEC_TYPE_ADPCM_G722,
        CODEC_TYPE_ADPCM_G726,
        CODEC_TYPE_ADPCM_G726LE,
        CODEC_TYPE_PCM_ALAW,
        CODEC_TYPE_PCM_MULAW,
    ];
    let lossless = !lossy.contains(&c);
    match (lossless, p.bits_per_sample) {
        (true, Some(1..=8)) => SampleFormat::Int8,
        (true, Some(9..=16)) => SampleFormat::Int16,
        (true, Some(17..=24)) => SampleFormat::Int24,
        (true, Some(25..=32)) => SampleFormat::Int32,
        _ => SampleFormat::Compressed,
    }
}

fn info_of(reader: &dyn FormatReader, detected: FileFormat) -> Result<(u32, AudioInfo)> {
    let track = reader
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioError::Unsupported("no decodable audio track".into()))?;
    let p = &track.codec_params;
    let sample_rate = p.sample_rate.filter(|&r| r > 0).ok_or_else(|| AudioError::Malformed("unknown sample rate".into()))?;
    let channels = p.channels.map(|c| c.count()).unwrap_or(0);
    let channels = u16::try_from(channels).map_err(|_| AudioError::Unsupported("too many channels".into()))?;
    Ok((
        track.id,
        AudioInfo {
            format: format_from_codec(p.codec, detected),
            sample_format: sample_format_of(p),
            sample_rate,
            channels,
            frames: p.n_frames.unwrap_or(0),
            bwf: None,
        },
    ))
}

/// Decode the whole first audio track. When `count_only`, samples are counted but not stored.
fn run(bytes: &[u8], detected: FileFormat, count_only: bool) -> Result<(AudioInfo, AudioBuffer)> {
    let mut reader = match open(bytes)? {
        Opened::Reader(reader) => reader,
        Opened::Empty(info) => {
            let buf = AudioBuffer::new(info.sample_rate, usize::from(info.channels), 0);
            return Ok((info, buf));
        }
    };
    let (track_id, mut info) = info_of(reader.as_ref(), detected)?;
    if let Some(n) = reader.tracks().iter().find(|t| t.id == track_id).and_then(|t| t.codec_params.n_frames) {
        let total = n.saturating_mul(u64::from(info.channels.max(1)));
        if !count_only && total > MAX_DECODE_SAMPLES {
            return Err(AudioError::TooLarge(format!("{total} samples exceeds the decode limit")));
        }
    }
    let params = reader
        .tracks()
        .iter()
        .find(|t| t.id == track_id)
        .map(|t| t.codec_params.clone())
        .ok_or_else(|| AudioError::Malformed("track vanished".into()))?;
    let alac = params.extra_data.as_deref().filter(|_| params.codec == CODEC_TYPE_ALAC).and_then(|d| be_u32(d, 0));
    if params.max_frames_per_packet.into_iter().chain(alac.map(u64::from)).any(|n| n > MAX_PACKET_FRAMES) {
        return Err(AudioError::Malformed("the file declares packets far longer than any codec uses".into()));
    }
    let mut decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(map_err)?;

    let mut planar: Vec<Vec<f32>> = Vec::new();
    let mut frames: u64 = 0;
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    let mut errors = 0usize;
    loop {
        let packet = match reader.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => {
                if frames == 0 {
                    return Err(map_err(e));
                }
                log::warn!("stopping decode after demux error: {e}");
                break;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(m)) => {
                errors += 1;
                log::debug!("skipping corrupt packet: {m}");
                if errors > 1000 && frames == 0 {
                    return Err(AudioError::Malformed(format!("no decodable packets: {m}")));
                }
                continue;
            }
            Err(SymError::IoError(_)) => break,
            Err(e) => return Err(map_err(e)),
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count();
        if ch == 0 {
            continue;
        }
        if planar.is_empty() {
            // The decoded stream is authoritative for the channel count.
            info.channels = u16::try_from(ch).map_err(|_| AudioError::Unsupported("too many channels".into()))?;
            planar = (0..ch).map(|_| Vec::new()).collect();
        }
        let n = decoded.frames() as u64;
        frames = frames.saturating_add(n);
        if frames.saturating_mul(ch as u64) > MAX_DECODE_SAMPLES {
            return Err(AudioError::TooLarge("decoded audio exceeds the 4 GiB limit".into()));
        }
        if count_only {
            continue;
        }
        let needed = decoded.capacity() as u64;
        let sb = match &mut sbuf {
            Some(sb) if sb.capacity() >= decoded.capacity() * ch => sb,
            _ => sbuf.insert(SampleBuffer::<f32>::new(needed, spec)),
        };
        sb.copy_interleaved_ref(decoded);
        for frame in sb.samples().chunks_exact(ch) {
            for (dst, &s) in planar.iter_mut().zip(frame) {
                dst.push(if s.is_finite() { s } else { 0.0 });
            }
        }
    }
    if info.channels == 0 {
        return Err(AudioError::Malformed("stream contains no audio".into()));
    }
    if planar.is_empty() {
        planar = (0..usize::from(info.channels)).map(|_| Vec::new()).collect();
    }
    info.frames = if count_only { frames } else { planar.iter().map(Vec::len).min().unwrap_or(0) as u64 };
    let sample_rate = info.sample_rate;
    Ok((info, AudioBuffer { sample_rate, channels: planar }))
}

pub(crate) fn probe(bytes: &[u8], detected: FileFormat) -> Result<AudioInfo> {
    guarded(|| {
        let reader = match open(bytes)? {
            Opened::Reader(reader) => reader,
            Opened::Empty(info) => return Ok(info),
        };
        let (_, info) = info_of(reader.as_ref(), detected)?;
        if info.frames > 0 && info.channels > 0 {
            return Ok(info);
        }
        // Length not in the header (e.g. MP3 without a Xing frame): count by decoding.
        run(bytes, detected, true).map(|(i, _)| i)
    })
}

pub(crate) fn decode(bytes: &[u8], detected: FileFormat) -> Result<(AudioInfo, AudioBuffer)> {
    guarded(|| run(bytes, detected, false))
}

/// Decode a checked native file incrementally. Accepts bounded MPEG audio and FLAC
/// streams; no generic probe is used, so unchecked CAF/MP4/FLAC/WAV metadata cannot reach a parser.
/// Leading ID3 tags are seek-skipped with a 1 MiB aggregate cap. Callback blocks hold at most
/// 4096 frames and eight channels (MPEG: 1152/two). The callback must consume each block before returning.
///
/// Other containers require their own bounded streaming preflight and return Unsupported.
/// The existing byte decoder remains available for browser inputs.
pub fn decode_file_blocks(mut file: std::fs::File, mut emit: impl FnMut(&AudioInfo, &AudioBuffer) -> Result<()>) -> Result<AudioInfo> {
    // Covers capped metadata, demuxer fragments/merge, decoder and conversion buffers.
    // FileEncoder output scratch is separately accounted by the consuming worker.
    let _scratch = crate::reader::ScratchReservation::new(4 * 1024 * 1024)?;
    guarded(|| {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(AudioError::Unsupported("audio source must be a regular file".into()));
        }
        let length = metadata.len();
        use std::io::{Read, Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        let mut marker = [0u8; 4];
        file.read_exact(&mut marker)?;
        if &marker == b"fLaC" {
            return decode_flac_file(file, &mut emit);
        }
        let base = mpeg_payload_offset(&mut file, length)?;
        let source = DecoderFileWindow { file, base, length: length.saturating_sub(base), position: 0, prefix: Vec::new(), quota: None };
        let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions { buffer_len: 64 * 1024 });
        let options = FormatOptions { enable_gapless: true, ..FormatOptions::default() };
        let mut reader = symphonia::default::formats::MpaReader::try_new(stream, &options).map_err(map_err)?;
        let (track_id, mut info) = info_of(&reader, FileFormat::Mp3)?;
        let params = reader
            .tracks()
            .iter()
            .find(|t| t.id == track_id)
            .map(|t| t.codec_params.clone())
            .ok_or_else(|| AudioError::Malformed("MPEG audio track vanished".into()))?;
        if ![CODEC_TYPE_MP1, CODEC_TYPE_MP2, CODEC_TYPE_MP3].contains(&params.codec)
            || !(1..=2).contains(&info.channels)
            || !(8_000..=48_000).contains(&info.sample_rate)
            || params.max_frames_per_packet.is_some_and(|n| n > 1152)
            || params.extra_data.as_ref().is_some_and(|data| data.len() > 4096)
        {
            return Err(AudioError::Unsupported("MPEG stream exceeds bounded decoder limits".into()));
        }
        let mut decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(map_err)?;
        let channels = usize::from(info.channels);
        let mut block = AudioBuffer { sample_rate: info.sample_rate, channels: (0..channels).map(|_| Vec::with_capacity(1152)).collect() };
        let mut scratch: Option<SampleBuffer<f32>> = None;
        let mut frames = 0u64;
        loop {
            let packet = match reader.next_packet() {
                Ok(packet) => packet,
                Err(SymError::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(map_err(error)),
            };
            if packet.track_id() != track_id {
                return Err(AudioError::Malformed("unexpected MPEG track change".into()));
            }
            if packet.buf().len() > 4096 || packet.dur() > 1152 {
                return Err(AudioError::TooLarge("MPEG packet exceeds bounded decoder limits".into()));
            }
            // Unlike whole-file import, never silently skip corrupt packets in a normalized file.
            let decoded = decoder.decode(&packet).map_err(map_err)?;
            let spec = *decoded.spec();
            if spec.channels.count() != channels || spec.rate != info.sample_rate {
                return Err(AudioError::Unsupported("MPEG channel/rate changes midstream".into()));
            }
            if decoded.capacity() > 1152 || decoded.frames() > 1152 {
                return Err(AudioError::TooLarge("MPEG decoded packet exceeds 1152 frames".into()));
            }
            let sample_buffer = match &mut scratch {
                Some(buffer) if buffer.capacity() >= decoded.capacity() * channels => buffer,
                _ => scratch.insert(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)),
            };
            let count = decoded.frames();
            sample_buffer.copy_interleaved_ref(decoded);
            for channel in &mut block.channels {
                channel.clear();
            }
            for frame in sample_buffer.samples().chunks_exact(channels) {
                for (output, value) in block.channels.iter_mut().zip(frame) {
                    output.push(if value.is_finite() { *value } else { 0.0 });
                }
            }
            frames = frames.checked_add(count as u64).ok_or_else(|| AudioError::TooLarge("decoded frame count overflow".into()))?;
            emit(&info, &block)?;
        }
        info.frames = frames;
        Ok(info)
    })
}

/// A seekable view whose logical offset zero follows the leading ID3 headers. This prevents
/// demuxer seeks from accidentally returning into skipped metadata, without allocating it.
struct DecoderFileWindow {
    file: std::fs::File,
    base: u64,
    length: u64,
    position: u64,
    // A checked metadata snapshot. FLAC parsers never see untrusted original metadata.
    prefix: Vec<u8>,
    quota: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}
impl std::io::Read for DecoderFileWindow {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Seek;
        let mut allowed = self.length.saturating_sub(self.position).min(output.len() as u64) as usize;
        if allowed == 0 {
            return Ok(0);
        }
        if let Some(quota) = &self.quota {
            let remaining = quota.load(std::sync::atomic::Ordering::Relaxed);
            if remaining == 0 {
                return Err(std::io::Error::other("bounded audio packet read quota exceeded"));
            }
            allowed = allowed.min(remaining);
        }
        let count = if self.position < self.prefix.len() as u64 {
            let start = self.position as usize;
            let count = allowed.min(self.prefix.len().saturating_sub(start));
            let source = self.prefix.get(start..start + count).ok_or_else(|| std::io::Error::other("metadata snapshot range"))?;
            output.get_mut(..count).ok_or_else(|| std::io::Error::other("metadata output range"))?.copy_from_slice(source);
            count
        } else {
            let offset = self.base.checked_add(self.position).ok_or_else(|| std::io::Error::other("audio position overflow"))?;
            self.file.seek(std::io::SeekFrom::Start(offset))?;
            let output = output.get_mut(..allowed).ok_or_else(|| std::io::Error::other("audio read size overflow"))?;
            std::io::Read::read(&mut self.file, output)?
        };
        if let Some(quota) = &self.quota {
            quota.fetch_sub(count, std::sync::atomic::Ordering::Relaxed);
        }
        self.position = self.position.saturating_add(count as u64);
        Ok(count)
    }
}
impl std::io::Seek for DecoderFileWindow {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let next = match to {
            std::io::SeekFrom::Start(position) => i128::from(position),
            std::io::SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
            std::io::SeekFrom::End(delta) => i128::from(self.length) + i128::from(delta),
        };
        self.position = u64::try_from(next).map_err(|_| std::io::Error::other("audio seek outside representable range"))?;
        Ok(self.position)
    }
}
impl symphonia::core::io::MediaSource for DecoderFileWindow {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.length)
    }
}

fn mpeg_payload_offset(input: &mut (impl std::io::Read + std::io::Seek), length: u64) -> Result<u64> {
    use std::io::SeekFrom;
    let mut offset = 0u64;
    loop {
        input.seek(SeekFrom::Start(offset))?;
        let mut head = [0u8; 10];
        let available = length.saturating_sub(offset).min(10) as usize;
        input.read_exact(head.get_mut(..available).ok_or_else(|| AudioError::Malformed("MPEG header size".into()))?)?;
        if head.get(..3) == Some(&b"ID3"[..]) {
            if available < 10 || !matches!(head.get(3), Some(2..=4)) || head.get(6..10).is_none_or(|bytes| bytes.iter().any(|b| *b & 0x80 != 0)) {
                return Err(AudioError::Malformed("invalid leading ID3 header".into()));
            }
            let size = head.get(6..10).unwrap_or_default().iter().fold(0u64, |n, b| (n << 7) | u64::from(*b));
            let footer = u64::from(head.get(3) == Some(&4) && head.get(5).is_some_and(|flags| *flags & 0x10 != 0)) * 10;
            offset = offset
                .checked_add(10)
                .and_then(|n| n.checked_add(size))
                .and_then(|n| n.checked_add(footer))
                .ok_or_else(|| AudioError::TooLarge("ID3 offset overflow".into()))?;
            if offset > 1024 * 1024 {
                return Err(AudioError::TooLarge("leading MPEG metadata exceeds 1 MiB".into()));
            }
            if offset > length {
                return Err(AudioError::Malformed("ID3 metadata exceeds file length".into()));
            }
            continue;
        }
        let b1 = head.get(1).copied().unwrap_or(0);
        let b2 = head.get(2).copied().unwrap_or(0);
        if available < 4
            || head.first() != Some(&0xff)
            || b1 & 0xe0 != 0xe0
            || (b1 >> 3) & 3 == 1
            || (b1 >> 1) & 3 == 0
            || b2 >> 4 == 0
            || b2 >> 4 == 15
            || (b2 >> 2) & 3 == 3
        {
            return Err(AudioError::Unsupported("bounded file decoding currently supports MPEG audio elementary streams only".into()));
        }
        return Ok(offset);
    }
}

#[cfg(test)]
mod file_stream_preflight_tests {
    use super::*;
    #[test]
    fn skips_capped_id3_without_copying_payload() {
        let bytes = [b"ID3\x04\x00\x00\x00\x00\x00\x04".as_slice(), &[0; 4], &[0xff, 0xfb, 0x90, 0]].concat();
        assert_eq!(mpeg_payload_offset(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap(), 14);
        let giant = b"ID3\x04\x00\x00\x7f\x7f\x7f\x7f";
        assert!(matches!(mpeg_payload_offset(&mut Cursor::new(giant), u64::MAX), Err(AudioError::TooLarge(_))));
    }
    #[test]
    fn other_containers_never_reach_generic_probe() {
        for magic in [b"caff".as_slice(), b"fLaC", b"RIFF", b"OggS", b"\x00\x00\x00\x18ftyp"] {
            assert!(matches!(mpeg_payload_offset(&mut Cursor::new(magic), magic.len() as u64), Err(AudioError::Unsupported(_))));
        }
        assert!(mpeg_payload_offset(&mut Cursor::new([0xff, 0xfb, 0, 0]), 4).is_err());
    }
}

const FLAC_METADATA_LIMIT: usize = 1024 * 1024;
const FLAC_PACKET_LIMIT: usize = 512 * 1024;
const FLAC_BLOCK_LIMIT: usize = 16_384;

/// Validate a bounded metadata snapshot, then retain only STREAMINFO for the decoder. Other
/// metadata remains in the original file but is deliberately outside the audio parser's view.
fn flac_prefix(input: &mut (impl std::io::Read + std::io::Seek), length: u64) -> Result<(Vec<u8>, u64, AudioInfo)> {
    use std::io::SeekFrom;
    input.seek(SeekFrom::Start(0))?;
    let mut prefix = Vec::with_capacity(FLAC_METADATA_LIMIT);
    let mut magic = [0u8; 4];
    input.read_exact(&mut magic)?;
    if &magic != b"fLaC" {
        return Err(AudioError::Malformed("missing FLAC marker".into()));
    }
    prefix.extend_from_slice(&magic);
    let mut blocks = 0usize;
    loop {
        blocks += 1;
        if blocks > 1024 {
            return Err(AudioError::TooLarge("too many FLAC metadata blocks".into()));
        }
        let mut header = [0u8; 4];
        input.read_exact(&mut header)?;
        let kind = header.first().copied().unwrap_or(0) & 0x7f;
        let size = header.get(1..4).unwrap_or_default().iter().fold(0usize, |n, b| (n << 8) | usize::from(*b));
        if (blocks == 1 && (kind != 0 || size != 34)) || (blocks > 1 && kind == 0) {
            return Err(AudioError::Malformed("FLAC requires one initial 34-byte STREAMINFO block".into()));
        }
        let end = prefix
            .len()
            .checked_add(4)
            .and_then(|n| n.checked_add(size))
            .filter(|n| *n <= FLAC_METADATA_LIMIT && *n as u64 <= length)
            .ok_or_else(|| AudioError::TooLarge("FLAC metadata exceeds the 1 MiB/file bound".into()))?;
        prefix.extend_from_slice(&header);
        let body = prefix.len();
        prefix.resize(end, 0);
        input.read_exact(prefix.get_mut(body..end).ok_or_else(|| AudioError::Malformed("FLAC metadata range".into()))?)?;
        if header.first().is_some_and(|byte| *byte & 0x80 != 0) {
            break;
        }
    }
    crate::flac::check_metadata(&prefix, 0)?;
    let info_bytes = prefix.get(8..42).ok_or_else(|| AudioError::Malformed("missing FLAC STREAMINFO".into()))?;
    let min_block = crate::pcm::be_u16(info_bytes, 0).ok_or_else(|| AudioError::Malformed("FLAC minimum block length".into()))?;
    let max_block = crate::pcm::be_u16(info_bytes, 2).ok_or_else(|| AudioError::Malformed("FLAC maximum block length".into()))?;
    let frame_min = info_bytes.get(4..7).unwrap_or_default().iter().fold(0usize, |n, b| (n << 8) | usize::from(*b));
    let frame_max = info_bytes.get(7..10).unwrap_or_default().iter().fold(0usize, |n, b| (n << 8) | usize::from(*b));
    let packed = crate::pcm::be_u64(info_bytes, 10).ok_or_else(|| AudioError::Malformed("FLAC sample format".into()))?;
    let sample_rate = (packed >> 44) as u32;
    let channels = ((packed >> 41) & 7) as u16 + 1;
    let bits = ((packed >> 36) & 31) as u32 + 1;
    let frames = packed & 0xf_ffff_ffff;
    if min_block < 16
        || max_block < min_block
        || usize::from(max_block) > FLAC_BLOCK_LIMIT
        || frame_min > FLAC_PACKET_LIMIT
        || frame_max > FLAC_PACKET_LIMIT
        || (frame_min != 0 && frame_max != 0 && frame_min > frame_max)
        || !(8_000..=655_350).contains(&sample_rate)
        || !(4..=32).contains(&bits)
        || !(1..=8).contains(&channels)
    {
        return Err(AudioError::Unsupported("FLAC format/frame bounds exceed streaming decoder limits".into()));
    }
    let sample_format = match bits {
        4..=8 => SampleFormat::Int8,
        9..=16 => SampleFormat::Int16,
        17..=24 => SampleFormat::Int24,
        _ => SampleFormat::Int32,
    };
    let metadata_end = prefix.len() as u64;
    // Synthetic STREAMINFO is the final and only metadata block; no other metadata is parsed.
    prefix.truncate(42);
    if let Some(header) = prefix.get_mut(4) {
        *header = 0x80;
    }
    // Release the 1 MiB metadata allocation before decoder/parser scratch is allocated.
    let compact = prefix.get(..42).ok_or_else(|| AudioError::Malformed("FLAC compact prefix".into()))?.to_vec();
    Ok((compact, metadata_end, AudioInfo { format: FileFormat::Flac, sample_format, sample_rate, channels, frames, bwf: None }))
}

fn decode_flac_file(mut file: std::fs::File, emit: &mut impl FnMut(&AudioInfo, &AudioBuffer) -> Result<()>) -> Result<AudioInfo> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let length = file.metadata()?.len();
    let (prefix, metadata_end, mut info) = flac_prefix(&mut file, length)?;
    if metadata_end == length && info.frames == 0 {
        return Ok(info);
    }
    let expected_frames = info.frames;
    let base = metadata_end.checked_sub(prefix.len() as u64).ok_or_else(|| AudioError::Malformed("FLAC prefix position".into()))?;
    let quota = Arc::new(AtomicUsize::new(2 * FLAC_PACKET_LIMIT));
    let source = DecoderFileWindow { file, base, length: length.saturating_sub(base), position: 0, prefix, quota: Some(Arc::clone(&quota)) };
    let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions { buffer_len: 64 * 1024 });
    let options = FormatOptions { enable_gapless: true, ..FormatOptions::default() };
    let mut reader = symphonia::default::formats::FlacReader::try_new(stream, &options).map_err(map_err)?;
    let params =
        reader.tracks().first().map(|track| track.codec_params.clone()).ok_or_else(|| AudioError::Malformed("FLAC has no audio track".into()))?;
    if params.codec != CODEC_TYPE_FLAC
        || params.max_frames_per_packet.is_some_and(|n| n > FLAC_BLOCK_LIMIT as u64)
        || params.extra_data.as_ref().is_none_or(|data| data.len() != 34)
    {
        return Err(AudioError::Malformed("FLAC decoder parameters escaped preflight bounds".into()));
    }
    let track_id = reader.tracks().first().map(|track| track.id).ok_or_else(|| AudioError::Malformed("FLAC track vanished".into()))?;
    let channels = usize::from(info.channels);
    let mut decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions { verify: true }).map_err(map_err)?;
    let mut scratch: Option<SampleBuffer<f32>> = None;
    let mut block = AudioBuffer { sample_rate: info.sample_rate, channels: (0..channels).map(|_| Vec::with_capacity(4096)).collect() };
    let mut frames = 0u64;
    loop {
        // A hostile unsynchronized frame cannot make the third-party scanner grow toward its
        // 16 MiB internal maximum. Each packet call may read at most 512 KiB plus buffered lookahead.
        quota.store(FLAC_PACKET_LIMIT, Ordering::Relaxed);
        let packet = match reader.next_packet() {
            Ok(packet) => packet,
            Err(SymError::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(map_err(error)),
        };
        if packet.track_id() != track_id || packet.buf().len() > FLAC_PACKET_LIMIT || packet.dur() > FLAC_BLOCK_LIMIT as u64 {
            return Err(AudioError::TooLarge("FLAC packet exceeds streaming bounds".into()));
        }
        let decoded = decoder.decode(&packet).map_err(map_err)?;
        let spec = *decoded.spec();
        let count = decoded.frames();
        if spec.channels.count() != channels || spec.rate != info.sample_rate || decoded.capacity() > FLAC_BLOCK_LIMIT || count > FLAC_BLOCK_LIMIT {
            return Err(AudioError::Malformed("FLAC decoded block differs from bounded format".into()));
        }
        let samples = match &mut scratch {
            Some(samples) if samples.capacity() >= decoded.capacity() * channels => samples,
            _ => scratch.insert(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)),
        };
        samples.copy_interleaved_ref(decoded);
        for start in (0..count).step_by(4096) {
            let end = (start + 4096).min(count);
            for output in &mut block.channels {
                output.clear();
            }
            let slice =
                samples.samples().get(start * channels..end * channels).ok_or_else(|| AudioError::Malformed("FLAC block sample range".into()))?;
            for frame in slice.chunks_exact(channels) {
                for (output, value) in block.channels.iter_mut().zip(frame) {
                    output.push(if value.is_finite() { *value } else { 0.0 });
                }
            }
            emit(&info, &block)?;
        }
        frames = frames.checked_add(count as u64).ok_or_else(|| AudioError::TooLarge("FLAC frame count overflow".into()))?;
    }
    if expected_frames != 0 && frames != expected_frames {
        return Err(AudioError::Malformed("FLAC decoded frame count differs from STREAMINFO".into()));
    }
    if decoder.finalize().verify_ok == Some(false) {
        return Err(AudioError::Malformed("FLAC decoded audio failed its integrity check".into()));
    }
    info.frames = frames;
    Ok(info)
}

#[cfg(test)]
mod flac_file_stream_tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn bounded_flac_blocks_match_native_reference() {
        for depth in [crate::BitDepth::Int16, crate::BitDepth::Int24, crate::BitDepth::Int32] {
            let input = AudioBuffer { sample_rate: 48_000, channels: vec![(0..5000).map(|n| ((n as f32) * 0.02).sin() * 0.7).collect()] };
            let encoded =
                crate::encode(&input, &crate::EncodeOptions { format: FileFormat::Flac, bit_depth: depth, dither: false, bwf: None }).unwrap();
            let expected = crate::decode(&encoded, Some("flac")).unwrap().1;
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("pa-flac-blocks-{}-{stamp}.flac", std::process::id()));
            let mut file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
            eprintln!("CREATED {} (retained)", path.display());
            file.write_all(&encoded).unwrap();
            let mut actual = Vec::new();
            let info = decode_file_blocks(file, |metadata, block| {
                assert_eq!(metadata.format, FileFormat::Flac);
                assert!(block.frames() <= 4096);
                actual.extend_from_slice(&block.channels[0]);
                Ok(())
            })
            .unwrap();
            assert_eq!(info.frames, 5000);
            assert_eq!(actual, expected.channels[0]);
        }
    }
    #[test]
    fn hostile_flac_metadata_is_rejected_before_decoder_construction() {
        let input = AudioBuffer { sample_rate: 48_000, channels: vec![vec![0.0; 64]] };
        let mut encoded =
            crate::encode(&input, &crate::EncodeOptions { format: FileFormat::Flac, bit_depth: crate::BitDepth::Int16, dither: false, bwf: None })
                .unwrap();
        encoded.get_mut(10..12).unwrap().copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(matches!(flac_prefix(&mut Cursor::new(&encoded), encoded.len() as u64), Err(AudioError::Unsupported(_))));
        let oversized = b"fLaC\x80\xff\xff\xff";
        assert!(flac_prefix(&mut Cursor::new(oversized), u64::MAX).is_err());
    }
}
