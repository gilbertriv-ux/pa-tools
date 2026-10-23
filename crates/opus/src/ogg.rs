//! A small Ogg page writer for one logical Opus stream (RFC 3533 pages, RFC 7845 mapping).
//!
//! Written from the RFC text. [`PageWriter`] packs whole packets into pages (never splitting one across
//! pages: an Opus packet is at most 1275 bytes), numbers the pages, flags the first page `BOS` and the
//! last `EOS`, and checksums each page with the Ogg CRC-32 ([`crc32`]).
//!
//! [`opus_head`] and [`opus_tags`] build the two header packets of RFC 7845 §5.

use std::io::{self, Write};

use crate::Error;

/// Longest packet this writer accepts: 255 lacing values of up to 255 bytes make one page body, and
/// Opus packets are far smaller. Bigger packets would have to be split across pages.
pub const MAX_PACKET: usize = 255 * 255 - 1;
/// Lacing values in one page header.
const MAX_SEGMENTS: usize = 255;
/// A page is closed once it holds this many packets (one second of 20 ms packets).
const PACKETS_PER_PAGE: usize = 50;

const HEADER_BOS: u8 = 0x02;
const HEADER_EOS: u8 = 0x04;

/// CRC-32 of RFC 3533 §6: polynomial 0x04C11DB7, initial value 0, not reflected, no final XOR.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, entry) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04C1_1DB7 } else { r << 1 };
            }
            *entry = r;
        }
        t
    });
    data.iter().fold(0u32, |crc, &b| (crc << 8) ^ table.get((crc >> 24) as usize ^ b as usize).copied().unwrap_or(0))
}

/// The `OpusHead` identification header (RFC 7845 §5.1), channel mapping family 0.
///
/// `input_rate` is the original sample rate, informational only; a player always decodes at 48 kHz.
pub fn opus_head(channels: u8, pre_skip: u16, input_rate: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(19);
    h.extend_from_slice(b"OpusHead");
    h.push(1); // version
    h.push(channels);
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&input_rate.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // mapping family 0
    h
}

/// Most bytes accepted in the vendor string or one comment.
pub const MAX_TAG_BYTES: usize = 1024;
/// Most comments accepted.
pub const MAX_TAGS: usize = 32;

/// The `OpusTags` comment header (RFC 7845 §5.2): a vendor string and `KEY=value` comments.
///
/// # Errors
/// [`Error::Internal`] when the vendor string or a comment is longer than [`MAX_TAG_BYTES`], there are
/// more than [`MAX_TAGS`] comments, or a key is empty or contains `=` or a control character.
pub fn opus_tags(vendor: &str, comments: &[(String, String)]) -> Result<Vec<u8>, Error> {
    if vendor.len() > MAX_TAG_BYTES || comments.len() > MAX_TAGS {
        return Err(Error::Internal("too many or too long Opus tags"));
    }
    let mut t = Vec::with_capacity(32 + vendor.len());
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    t.extend_from_slice(vendor.as_bytes());
    t.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for (key, value) in comments {
        let key_ok = !key.is_empty() && key.bytes().all(|b| (0x20..=0x7D).contains(&b) && b != b'=');
        if !key_ok || key.len() + 1 + value.len() > MAX_TAG_BYTES {
            return Err(Error::Internal("invalid Opus tag"));
        }
        t.extend_from_slice(&((key.len() + 1 + value.len()) as u32).to_le_bytes());
        t.extend_from_slice(key.as_bytes());
        t.push(b'=');
        t.extend_from_slice(value.as_bytes());
    }
    Ok(t)
}

/// Writes the pages of one logical bitstream.
///
/// Packets are queued with [`packet`](Self::packet) and leave in a page when it is full or when
/// [`end_page`](Self::end_page) or [`finish`](Self::finish) is called. The last page is held back until
/// `finish`, so it can carry the end-of-stream flag.
pub struct PageWriter<W: Write> {
    out: W,
    serial: u32,
    sequence: u32,
    /// Lacing values of the page being built.
    lacing: Vec<u8>,
    body: Vec<u8>,
    packets: usize,
    /// Granule position after the last queued packet (the page's granule position).
    granule: i64,
    first: bool,
}

impl<W: Write> PageWriter<W> {
    pub fn new(out: W, serial: u32) -> Self {
        PageWriter { out, serial, sequence: 0, lacing: Vec::new(), body: Vec::new(), packets: 0, granule: 0, first: true }
    }

    /// Queues one whole packet whose last sample is at `granule` (RFC 7845 §4: 48 kHz samples, pre-skip
    /// included).
    ///
    /// # Errors
    /// [`Error::Internal`] for a packet longer than [`MAX_PACKET`]; [`Error::Io`] when a full page cannot
    /// be written.
    pub fn packet(&mut self, data: &[u8], granule: i64) -> Result<(), Error> {
        if data.len() > MAX_PACKET {
            return Err(Error::Internal("Ogg packet too large"));
        }
        // 255-byte multiples need a terminating 0 lacing value, so a packet takes len / 255 + 1 values.
        let segments = data.len() / 255 + 1;
        if self.packets >= PACKETS_PER_PAGE || self.lacing.len() + segments > MAX_SEGMENTS {
            self.write_page(0)?;
        }
        let mut left = data.len();
        while left >= 255 {
            self.lacing.push(255);
            left -= 255;
        }
        self.lacing.push(left as u8);
        self.body.extend_from_slice(data);
        self.packets += 1;
        self.granule = granule;
        Ok(())
    }

    /// Writes the queued packets as a page now (used after each header packet, which must end its page).
    pub fn end_page(&mut self) -> Result<(), Error> {
        if self.packets > 0 { self.write_page(0) } else { Ok(()) }
    }

    /// Writes the last page with the end-of-stream flag and returns the sink. A stream with no packet
    /// at all gets an empty end-of-stream page.
    pub fn finish(mut self) -> Result<W, Error> {
        self.write_page(HEADER_EOS)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn write_page(&mut self, flags: u8) -> Result<(), Error> {
        let mut page = Vec::with_capacity(27 + self.lacing.len() + self.body.len());
        page.extend_from_slice(b"OggS");
        page.push(0); // stream structure version
        page.push(flags | if self.first { HEADER_BOS } else { 0 });
        page.extend_from_slice(&self.granule.to_le_bytes());
        page.extend_from_slice(&self.serial.to_le_bytes());
        page.extend_from_slice(&self.sequence.to_le_bytes());
        page.extend_from_slice(&[0; 4]); // checksum, filled in below
        page.push(self.lacing.len() as u8);
        page.extend_from_slice(&self.lacing);
        page.extend_from_slice(&self.body);
        let crc = crc32(&page).to_le_bytes();
        page.splice(22..26, crc);
        self.out.write_all(&page).map_err(|e: io::Error| Error::Io(e.to_string()))?;
        self.sequence = self.sequence.checked_add(1).ok_or(Error::TooLong)?;
        self.first = false;
        self.lacing.clear();
        self.body.clear();
        self.packets = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-by-bit reference for the same CRC.
    fn crc_slow(data: &[u8]) -> u32 {
        let mut crc = 0u32;
        for &b in data {
            crc ^= u32::from(b) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
            }
        }
        crc
    }

    #[test]
    fn crc_matches_the_bitwise_definition() {
        assert_eq!(crc32(b""), 0);
        for data in [&b"OggS"[..], &[0u8; 300][..], &(0..=255u8).collect::<Vec<_>>()[..]] {
            assert_eq!(crc32(data), crc_slow(data));
        }
        // Known value: the CRC-32/MPEG-2 check string without the final inversion Ogg also omits.
        assert_eq!(crc32(b"123456789"), 0x89A1_897F);
    }

    #[test]
    fn head_and_tags_layout() {
        let h = opus_head(2, 120, 44_100);
        assert_eq!(h.len(), 19);
        assert_eq!(&h[..9], b"OpusHead\x01");
        assert_eq!((h[9], u16::from_le_bytes([h[10], h[11]]), u32::from_le_bytes([h[12], h[13], h[14], h[15]])), (2, 120, 44_100));
        let t = opus_tags("v", &[("TITLE".into(), "x".into())]).unwrap();
        assert_eq!(&t[..8], b"OpusTags");
        assert_eq!(&t[8..], &[1, 0, 0, 0, b'v', 1, 0, 0, 0, 7, 0, 0, 0, b'T', b'I', b'T', b'L', b'E', b'=', b'x']);
        assert!(opus_tags("", &[("A=B".into(), "x".into())]).is_err());
        assert!(opus_tags("", &[(String::new(), "x".into())]).is_err());
        assert!(opus_tags(&"v".repeat(MAX_TAG_BYTES + 1), &[]).is_err());
    }

    #[test]
    fn pages_are_well_formed() {
        let mut w = PageWriter::new(Vec::new(), 7);
        w.packet(b"head", 0).unwrap();
        w.end_page().unwrap();
        // 255 and 510 byte packets need a terminating zero lacing value
        w.packet(&[1; 255], 960).unwrap();
        w.packet(&[2; 510], 1920).unwrap();
        w.packet(&[3; 0], 2880).unwrap();
        let out = w.finish().unwrap();
        assert_eq!(&out[..4], b"OggS");
        assert_eq!(out[5], HEADER_BOS);
        // second page: lacing 255,0, 255,255,0, 0
        let p2 = 27 + 1 + 4;
        assert_eq!(&out[p2..p2 + 4], b"OggS");
        assert_eq!(out[p2 + 5], HEADER_EOS);
        assert_eq!(&out[p2 + 6..p2 + 14], &2880i64.to_le_bytes());
        assert_eq!(out[p2 + 26], 6);
        assert_eq!(&out[p2 + 27..p2 + 33], &[255, 0, 255, 255, 0, 0]);
        // checksum verifies
        let page2 = &out[p2..];
        let mut zeroed = page2.to_vec();
        zeroed.splice(22..26, [0; 4]);
        assert_eq!(&page2[22..26], &crc32(&zeroed).to_le_bytes());
    }

    #[test]
    fn full_pages_roll_over_and_the_last_one_is_eos() {
        let mut w = PageWriter::new(Vec::new(), 1);
        for k in 0..120 {
            w.packet(&[0u8; 300], (k + 1) * 960).unwrap();
        }
        let out = w.finish().unwrap();
        let pages = out.windows(4).filter(|w| *w == b"OggS").count();
        // 300-byte packets take 2 lacing values; the 50-packet limit binds before 255 values do
        assert_eq!(pages, 3);
    }

    #[test]
    fn oversized_packets_are_refused() {
        let mut w = PageWriter::new(Vec::new(), 1);
        assert!(w.packet(&vec![0; MAX_PACKET + 1], 0).is_err());
        assert!(w.packet(&vec![0; MAX_PACKET], 0).is_ok());
    }
}
