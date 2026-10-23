//! Checked field reads. Every offset and length in an interchange file is hostile input, so no
//! reader indexes a slice directly: these helpers return `None` when a field is not wholly inside
//! the buffer (including when `at + len` would overflow).

/// The longest string kept from a file, in characters (names and comments are far shorter; the cap
/// bounds what a hostile property can make every later copy of it cost).
pub(crate) const MAX_STRING: usize = 4096;

/// `len` bytes at `at`, or `None` if they are not all inside `b`.
pub(crate) fn slice(b: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    b.get(at..at.checked_add(len)?)
}

/// `N` bytes at `at` as an array.
pub(crate) fn array<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    slice(b, at, N)?.first_chunk::<N>().copied()
}

pub(crate) fn le16(b: &[u8], at: usize) -> Option<u16> {
    array::<2>(b, at).map(u16::from_le_bytes)
}

pub(crate) fn le32(b: &[u8], at: usize) -> Option<u32> {
    array::<4>(b, at).map(u32::from_le_bytes)
}

pub(crate) fn le_i32(b: &[u8], at: usize) -> Option<i32> {
    array::<4>(b, at).map(i32::from_le_bytes)
}

pub(crate) fn le_i64(b: &[u8], at: usize) -> Option<i64> {
    array::<8>(b, at).map(i64::from_le_bytes)
}

pub(crate) fn be16(b: &[u8], at: usize) -> Option<u16> {
    array::<2>(b, at).map(u16::from_be_bytes)
}

pub(crate) fn be32(b: &[u8], at: usize) -> Option<u32> {
    array::<4>(b, at).map(u32::from_be_bytes)
}
