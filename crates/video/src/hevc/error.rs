use crate::bitstream::BitError;

/// Errors produced by the decoder.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The bitstream ended early or contained an invalid Exp-Golomb code.
    #[error("bitstream error: {0}")]
    Bitstream(#[from] BitError),
    /// A syntax element had a value outside its legal range.
    #[error("invalid bitstream: {0}")]
    Invalid(String),
    /// A valid feature this decoder does not implement (yet).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A slice referenced a parameter set that has not been received.
    #[error("missing parameter set: {0}")]
    MissingParameterSet(String),
    /// A bug in this decoder (a panic caught while decoding a picture). Never produced by a
    /// well-formed or by a merely damaged stream; the hostile-input tests assert it never appears.
    #[error("internal decoder error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}

/// `v + k` for a coded `*_minus*` field, or the error that field's range check would give (a
/// value at the top of the `u32` range is out of range for every such field).
pub(crate) fn plus(v: u32, k: u32, what: &str) -> Result<u32> {
    v.checked_add(k).ok_or_else(|| Error::Invalid(format!("{what} out of range")))
}

pub(crate) fn unsupported<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Unsupported(msg.into()))
}

/// `ensure!(cond, "message")` returns `Error::Invalid` when `cond` is false.
macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err($crate::hevc::error::Error::Invalid(format!($($arg)+)));
        }
    };
}
pub(crate) use ensure;
