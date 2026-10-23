//! MSB-first bit reader for OBU headers and the uncompressed frame header (spec 4.10 descriptors).

use crate::av1::{Error, Result};

#[derive(Clone)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// `get_position()`: bits consumed.
    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn byte_pos(&self) -> usize {
        self.pos.div_ceil(8)
    }

    #[inline]
    pub fn bit(&mut self) -> Result<u32> {
        let byte = *self.data.get(self.pos >> 3).ok_or(Error::Truncated)?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(b as u32)
    }

    /// `f(n)`, n ≤ 32.
    pub fn f(&mut self, n: u32) -> Result<u32> {
        let mut x: u64 = 0;
        for _ in 0..n {
            x = (x << 1) | self.bit()? as u64;
        }
        Ok(x as u32)
    }

    pub fn flag(&mut self) -> Result<bool> {
        Ok(self.bit()? == 1)
    }

    /// `uvlc()`
    pub fn uvlc(&mut self) -> Result<u32> {
        let mut leading = 0u32;
        while self.bit()? == 0 {
            leading += 1;
            if leading > 40 {
                return Err(Error::Invalid("uvlc"));
            }
        }
        if leading >= 32 {
            return Ok(u32::MAX);
        }
        let v = self.f(leading)?;
        Ok(v + ((1u64 << leading) - 1) as u32)
    }

    /// `le(n)`
    pub fn le(&mut self, n: u32) -> Result<u32> {
        let mut t = 0u32;
        for i in 0..n {
            t |= self.f(8)?.checked_shl(8 * i).unwrap_or(0);
        }
        Ok(t)
    }

    /// `leb128()`
    pub fn leb128(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for i in 0..8 {
            let b = u64::from(self.f(8)?);
            v |= (b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(Error::Invalid("leb128 longer than 8 bytes"))
    }

    /// `su(n)`
    pub fn su(&mut self, n: u32) -> Result<i32> {
        if n == 0 {
            return Ok(0);
        }
        let v = i64::from(self.f(n)?);
        let sign = 1i64 << (n.min(32) - 1);
        Ok(if v & sign != 0 { (v - 2 * sign) as i32 } else { v as i32 })
    }

    /// `ns(n)`
    pub fn ns(&mut self, n: u32) -> Result<u32> {
        if n <= 1 {
            return Ok(0);
        }
        let w = 32 - n.leading_zeros(); // FloorLog2(n) + 1
        let m = (1u64 << w) - u64::from(n);
        let v = self.f(w - 1)?;
        if u64::from(v) < m {
            return Ok(v);
        }
        let extra = self.bit()?;
        // v < 2^(w-1) <= n, so the result is below 2n and fits when n < 2^31 (callers pass tiny values).
        Ok(((u64::from(v) << 1) - m + u64::from(extra)).min(u64::from(u32::MAX)) as u32)
    }

    /// `byte_alignment()`
    pub fn byte_align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// Read a leb128 from a byte slice; returns (value, bytes used). At most 8 bytes (the spec's limit).
pub(crate) fn leb128(data: &[u8]) -> Result<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..8 {
        let b = *data.get(i).ok_or(Error::Truncated)? as u64;
        v |= (b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Err(Error::Invalid("leb128 longer than 8 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_most_significant_first_and_runs_dry_cleanly() {
        let mut r = BitReader::new(&[0b1010_0000, 0xff]);
        assert_eq!(r.f(3).ok(), Some(0b101));
        assert_eq!(r.f(13).ok(), Some(0b0_0000_1111_1111));
        assert!(matches!(r.bit(), Err(Error::Truncated)));
        assert!(matches!(r.f(32), Err(Error::Truncated)));
    }

    #[test]
    fn su_with_zero_width_is_zero_and_full_width_does_not_overflow() {
        let data = [0xff; 8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.su(0).ok(), Some(0));
        assert_eq!(r.position(), 0);
        assert_eq!(r.su(1).ok(), Some(-1));
        assert_eq!(r.su(32).ok(), Some(-1));
        let mut r = BitReader::new(&[0x7f, 0xff, 0xff, 0xff]);
        assert_eq!(r.su(32).ok(), Some(i32::MAX));
    }

    #[test]
    fn ns_handles_the_extremes_without_overflow() {
        let mut r = BitReader::new(&[0xff; 16]);
        assert_eq!(r.ns(0).ok(), Some(0));
        assert_eq!(r.ns(1).ok(), Some(0));
        // A width near the top of the range: the result is clamped, never wrapped or panicking.
        assert!(r.ns(u32::MAX).is_ok());
        assert!(r.ns(1 << 31).is_ok());
        let mut r = BitReader::new(&[0b1000_0000]);
        assert_eq!(r.ns(5).ok(), Some(2));
        let mut r = BitReader::new(&[0b1110_0000]);
        assert_eq!(r.ns(5).ok(), Some(4));
    }

    #[test]
    fn uvlc_stops_at_forty_leading_zeros() {
        let mut r = BitReader::new(&[0; 8]);
        assert!(matches!(r.uvlc(), Err(Error::Invalid(_))));
        // 39 zeros then a one: more than 32 leading zeros is the largest value, not an error.
        let mut r = BitReader::new(&[0, 0, 0, 0, 0x01, 0, 0, 0]);
        assert_eq!(r.uvlc().ok(), Some(u32::MAX));
    }

    #[test]
    fn le_wider_than_four_bytes_does_not_shift_out_of_range() {
        let mut r = BitReader::new(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(r.le(6).ok(), Some(0x0403_0201));
    }

    #[test]
    fn leb128_reads_up_to_eight_bytes_and_refuses_a_ninth() {
        assert_eq!(leb128(&[0x05]).ok(), Some((5, 1)));
        assert_eq!(leb128(&[0xe5, 0x8e, 0x26]).ok(), Some((624_485, 3)));
        assert_eq!(leb128(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01]).ok(), Some((1 << 49, 8)));
        assert!(matches!(leb128(&[0x80; 9]), Err(Error::Invalid(_))));
        assert!(matches!(leb128(&[0xff; 12]), Err(Error::Invalid(_))));
        assert!(matches!(leb128(&[0x80, 0x80]), Err(Error::Truncated)));
        assert!(matches!(leb128(&[]), Err(Error::Truncated)));
        let mut r = BitReader::new(&[0xe5, 0x8e, 0x26]);
        assert_eq!(r.leb128().ok(), Some(624_485));
        let mut r = BitReader::new(&[0xff; 9]);
        assert!(r.leb128().is_err());
    }
}
