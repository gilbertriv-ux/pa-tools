//! The build without the `aac-encode` feature: the same API, every call answering [`Error::NotBuilt`].
//! A caller needs no `cfg` of its own; it shows the error to the person ("AAC export is not included in
//! this build") and offers another format.

use std::convert::Infallible;
use std::io::{Seek, Write};
use std::marker::PhantomData;

use super::{AacOptions, Stats};
use crate::Error;

/// Encodes interleaved audio to an `.m4a` file. Always [`Error::NotBuilt`] in this build.
///
/// # Errors
/// [`Error::NotBuilt`].
pub fn encode_m4a(_interleaved: &[f32], _channels: u16, _sample_rate: u32, _bitrate_bps: u32) -> Result<Vec<u8>, Error> {
    Err(Error::NotBuilt)
}

/// [`encode_m4a`] with [`AacOptions`]. Always [`Error::NotBuilt`] in this build.
///
/// # Errors
/// [`Error::NotBuilt`].
pub fn encode_m4a_with(_interleaved: &[f32], _channels: u16, _sample_rate: u32, _options: &AacOptions) -> Result<(Vec<u8>, Stats), Error> {
    Err(Error::NotBuilt)
}

/// Encodes interleaved audio to an ADTS `.aac` stream. Always [`Error::NotBuilt`] in this build.
///
/// # Errors
/// [`Error::NotBuilt`].
pub fn encode_adts(_interleaved: &[f32], _channels: u16, _sample_rate: u32, _bitrate_bps: u32) -> Result<Vec<u8>, Error> {
    Err(Error::NotBuilt)
}

/// [`encode_adts`] with [`AacOptions`]. Always [`Error::NotBuilt`] in this build.
///
/// # Errors
/// [`Error::NotBuilt`].
pub fn encode_adts_with(_interleaved: &[f32], _channels: u16, _sample_rate: u32, _options: &AacOptions) -> Result<(Vec<u8>, Stats), Error> {
    Err(Error::NotBuilt)
}

/// Streams interleaved audio into an `.m4a` file. Cannot be created in this build.
pub struct M4aWriter<W: Write + Seek> {
    never: Infallible,
    _out: PhantomData<W>,
}

impl<W: Write + Seek> M4aWriter<W> {
    /// # Errors
    /// [`Error::NotBuilt`].
    pub fn new(_out: W, _channels: u16, _sample_rate: u32, _options: &AacOptions) -> Result<Self, Error> {
        Err(Error::NotBuilt)
    }

    /// What has been written so far.
    pub fn stats(&self) -> Stats {
        match self.never {}
    }

    /// # Errors
    /// [`Error::NotBuilt`].
    pub fn write_interleaved(&mut self, _samples: &[f32]) -> Result<(), Error> {
        match self.never {}
    }

    /// # Errors
    /// [`Error::NotBuilt`].
    pub fn finish(self) -> Result<(W, Stats), Error> {
        match self.never {}
    }
}

/// Streams interleaved audio into an ADTS `.aac` stream. Cannot be created in this build.
pub struct AdtsWriter<W: Write> {
    never: Infallible,
    _out: PhantomData<W>,
}

impl<W: Write> AdtsWriter<W> {
    /// # Errors
    /// [`Error::NotBuilt`].
    pub fn new(_out: W, _channels: u16, _sample_rate: u32, _options: &AacOptions) -> Result<Self, Error> {
        Err(Error::NotBuilt)
    }

    /// What has been written so far.
    pub fn stats(&self) -> Stats {
        match self.never {}
    }

    /// # Errors
    /// [`Error::NotBuilt`].
    pub fn write_interleaved(&mut self, _samples: &[f32]) -> Result<(), Error> {
        match self.never {}
    }

    /// # Errors
    /// [`Error::NotBuilt`].
    pub fn finish(self) -> Result<(W, Stats), Error> {
        match self.never {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn every_entry_point_says_not_built() {
        let opts = AacOptions::new(128_000);
        assert_eq!(encode_m4a(&[0.0; 4], 2, 48_000, 128_000), Err(Error::NotBuilt));
        assert_eq!(encode_m4a_with(&[0.0; 4], 2, 48_000, &opts), Err(Error::NotBuilt));
        assert_eq!(encode_adts(&[0.0; 4], 2, 48_000, 128_000), Err(Error::NotBuilt));
        assert_eq!(encode_adts_with(&[0.0; 4], 2, 48_000, &opts), Err(Error::NotBuilt));
        assert!(matches!(M4aWriter::new(Cursor::new(Vec::new()), 2, 48_000, &opts), Err(Error::NotBuilt)));
        assert!(matches!(AdtsWriter::new(Vec::new(), 2, 48_000, &opts), Err(Error::NotBuilt)));
        // arguments that would be invalid are still just "not built": the build, not the input, is the problem
        assert_eq!(encode_m4a(&[], 0, 0, 0), Err(Error::NotBuilt));
    }
}
