//! The default build (no `aac-encode` feature): the same API, every call answering `Error::NotBuilt`,
//! so the application needs no `cfg` of its own. Run with `cargo test -p soundcraft-aac`.

#![cfg(not(feature = "aac-encode"))]

use std::io::Cursor;

use soundcraft_aac::{
    AacOptions, AdtsWriter, ENCODER_BUILT, EXPORT_SAMPLE_RATES, Error, M4aWriter, MAX_BITRATE_PER_CHANNEL, MIN_BITRATE_PER_CHANNEL, bitrate_range,
    encode_adts, encode_adts_with, encode_m4a, encode_m4a_with,
};

#[test]
fn the_encoder_is_not_built() {
    const { assert!(!ENCODER_BUILT) };
}

#[test]
fn every_entry_point_says_not_built() {
    let x = vec![0.1f32; 4096];
    let opts = AacOptions::new(128_000);
    assert_eq!(encode_m4a(&x, 2, 48_000, 128_000), Err(Error::NotBuilt));
    assert_eq!(encode_m4a_with(&x, 2, 48_000, &opts), Err(Error::NotBuilt));
    assert_eq!(encode_adts(&x, 2, 48_000, 128_000), Err(Error::NotBuilt));
    assert_eq!(encode_adts_with(&x, 2, 48_000, &opts), Err(Error::NotBuilt));
    assert!(matches!(M4aWriter::new(Cursor::new(Vec::new()), 2, 48_000, &opts), Err(Error::NotBuilt)));
    assert!(matches!(AdtsWriter::new(Vec::new(), 2, 48_000, &opts), Err(Error::NotBuilt)));
    // hostile arguments get the same answer: the build, not the input, is what fails
    assert_eq!(encode_m4a(&[], 0, 0, 0), Err(Error::NotBuilt));
    assert_eq!(encode_adts(&[f32::NAN; 3], 65_535, u32::MAX, u32::MAX), Err(Error::NotBuilt));
}

#[test]
fn the_message_is_one_a_person_can_read() {
    assert_eq!(Error::NotBuilt.to_string(), "AAC export is not included in this build");
}

#[test]
fn the_format_limits_are_still_available_for_a_dialog() {
    assert_eq!(EXPORT_SAMPLE_RATES.len(), 13);
    assert_eq!(bitrate_range(2, 44_100), Ok((2 * MIN_BITRATE_PER_CHANNEL, 2 * 264_600)));
    assert_eq!(bitrate_range(1, 96_000), Ok((MIN_BITRATE_PER_CHANNEL, MAX_BITRATE_PER_CHANNEL)));
    assert_eq!(bitrate_range(3, 48_000), Err(Error::InvalidChannels(3)));
}
