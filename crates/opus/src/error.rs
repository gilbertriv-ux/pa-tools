//! Errors of the Opus encoder and the Ogg Opus writer.

use std::fmt;

/// Why an encode was refused or failed. Nothing in this crate panics on bad input; it returns one of
/// these instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Opus streams written here carry one or two channels.
    InvalidChannels(u16),
    /// The input sample rate is outside the supported range ([`crate::MIN_SAMPLE_RATE`] to
    /// [`crate::MAX_SAMPLE_RATE`]).
    InvalidSampleRate(u32),
    /// The bitrate (bits per second for all channels together) is outside `min..=max`.
    InvalidBitrate { bitrate: u32, min: u32, max: u32 },
    /// The mode and bandwidth do not form a valid Opus coding configuration.
    InvalidMode,
    /// An interleaved buffer's length is not a whole number of frames.
    Misaligned { len: usize, channels: usize },
    /// A frame passed to the packet encoder has the wrong number of samples.
    FrameSize { expected: usize, got: usize },
    /// There is no audio to encode.
    Empty,
    /// The audio is longer than [`crate::MAX_SECONDS`].
    TooLong,
    /// The sink refused the bytes.
    Io(String),
    /// The writer was already finished.
    Finished,
    /// An internal table could not be built (cannot happen with the fixed frame size).
    Internal(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidChannels(n) => write!(f, "Opus export takes 1 or 2 channels, not {n}"),
            Error::InvalidSampleRate(r) => write!(f, "sample rate {r} Hz is outside {} to {} Hz", crate::MIN_SAMPLE_RATE, crate::MAX_SAMPLE_RATE),
            Error::InvalidBitrate { bitrate, min, max } => write!(f, "bitrate {bitrate} bit/s is outside {min} to {max} bit/s"),
            Error::InvalidMode => write!(f, "unsupported Opus mode and bandwidth combination"),
            Error::Misaligned { len, channels } => write!(f, "{len} samples are not a whole number of {channels}-channel frames"),
            Error::FrameSize { expected, got } => write!(f, "an Opus frame is {expected} samples, got {got}"),
            Error::Empty => write!(f, "there is no audio to encode"),
            Error::TooLong => write!(f, "the audio is longer than {} hours", crate::MAX_SECONDS / 3600),
            Error::Io(e) => write!(f, "write failed: {e}"),
            Error::Finished => write!(f, "the writer is already finished"),
            Error::Internal(what) => write!(f, "internal error: {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}
