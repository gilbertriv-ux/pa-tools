# Audio codecs

> **Last reviewed:** 2026-10-23 · **Last updated:** 2026-10-23 · **Change:** major (created: native Ogg Opus and AAC export crates for G-21)

What `crates/opus` (package `soundcraft-opus`) and `crates/aac` (package `soundcraft-aac`) can write, which
of them ship, and why AAC is compiled out of shipped builds. For the format table see
[`file-format-parity.md`](file-format-parity.md); for what is still missing see G-21 in [`gaps.md`](gaps.md).
For the video side, see `docs/video-codecs.md` (added with the video decoders, G-17).

**Status.** These are crates, tested on their own. Bounce, the CLI and the control channel do not call them
yet: that wiring is a later lane (see "Wiring" below). Nothing in `crates/engine`, `crates/mix`,
`crates/ui-egui` or `crates/cli` changed.

## What exports

| Format | Crate | Build | One-shot | Streaming |
|---|---|---|---|---|
| Ogg Opus (`.opus`) | `soundcraft-opus` | always | `encode_opus_ogg`, `encode_opus_ogg_with` | `OpusOggWriter` |
| AAC-LC in MPEG-4 audio (`.m4a`) | `soundcraft-aac` | Cargo feature `aac-encode` | `encode_m4a`, `encode_m4a_with` | `M4aWriter` |
| AAC-LC in ADTS (`.aac`) | `soundcraft-aac` | Cargo feature `aac-encode` | `encode_adts`, `encode_adts_with` | `AdtsWriter` |

MP3 is not part of this work and is still open under G-21. Opus ships enabled with no feature. AAC is
written, tested and kept in the tree but compiled out of shipped builds, see "Patent posture".

All of them take **interleaved `f32`** (nominally -1.0 to 1.0), `channels: u16`, `sample_rate: u32` and
`bitrate_bps: u32` (the total for all channels), and return `Result<_, Error>`:

```rust
let file: Vec<u8> = soundcraft_opus::encode_opus_ogg(&samples, 2, 44_100, 128_000)?;
let file: Vec<u8> = soundcraft_aac::encode_m4a(&samples, 2, 44_100, 128_000)?;   // Error::NotBuilt without aac-encode
```

The `_with` variants take options (tags, and for Opus the application and vendor) and also return what the
encode did (`Stats`: frames in, frames played, packets or access units, and how many samples were NaN or
beyond full scale). The streaming writers (`write_interleaved` any number of times, then `finish`) write the
audio out as it is encoded, so they hold no more than a packet or a second of it; they are what a bounce with
progress and cancel needs, and their output is byte-identical to the one-shot call whatever the chunk size.
Memory is flat for Opus (about one second of packets) and for AAC in ADTS, but **not quite flat for `.m4a`**: the
`stsz` and `stss` sample tables for the `moov` box are kept until `finish`, about 8 bytes per access unit, which is
roughly 32 MB per 24 hours of 48 kHz audio (1024 samples per access unit).

### Validation

Every input is checked before anything is encoded, and a bad one is an `Error`, never a panic.

| Input | Opus | AAC |
|---|---|---|
| Channels | 1 or 2 | 1 or 2 |
| Sample rate | 8 000 to 192 000 Hz; anything but 48 kHz is converted to 48 kHz first (Kaiser-windowed sinc, no latency) | one of the 13 AAC rates: 96 000, 88 200, 64 000, 48 000, 44 100, 32 000, 24 000, 22 050, 16 000, 12 000, 11 025, 8 000, 7 350 |
| Bitrate (total) | 6 kb/s per channel to 510 kb/s | 8 kb/s per channel to the lower of 320 kb/s and 6 times the sample rate, per channel (`bitrate_range` gives the pair) |
| Length | 1 frame to 24 hours | 1 frame to 24 hours |
| Empty input | `Error::Empty` | `Error::Empty` |
| Not a whole number of frames | `Error::Misaligned` | `Error::Misaligned` |
| NaN, infinity | silence, counted | silence, counted |
| Beyond full scale | clamped to full scale, counted | clamped to full scale, counted |

Three channels or more are refused: how a session's surround layout maps to a file's channel order is a
decision for the wiring (see below), not for the codec crates.

## Timing

The two codecs delay the signal and pad the end; the files say so, and a player reproduces the input length.

- **Ogg Opus.** `OpusHead` carries a pre-skip of 120 samples (48 kHz). Granule positions count every decoded
  sample including the pre-skip, so packet `k` ends at `k * 960`, and the last packet's granule is
  `pre_skip + frames_out`. The decoded, playable length is `final granule - pre_skip` = `frames_out`, which for
  a 48 kHz input is the input length and for another rate is `round(frames * 48000 / rate)`. ffprobe's
  container duration is the last granule, so it includes the pre-skip (measured, 120 / 48000 s = 2.5 ms).
- **AAC in `.m4a`.** The encoder delay is 1024 samples and the tail is padded to a whole frame. The file
  has `ceil(n / 1024) + 1` access units and an `elst` edit list `{ media_time: 1024, segment_duration: n }`,
  with the movie timescale equal to the sample rate so `n` is an exact sample count.
- **AAC in ADTS.** No delay information is possible in the format. A player starts the stream with the
  1024 priming samples and plays the padded tail. The access units are byte-identical to the `.m4a`'s.

## Measured

Signal: a 1 kHz sine at -14 dBFS peak plus pink noise, 3 s, per channel. Loudness is BS.1770 integrated, from the
meter in `soundcraft-dsp`. Decoders: this repository's own clean-room decoders (`soundcraft-aac`,
`soundcraft-opus-decode`), plus a second, independent decoder for each.

| | Round trip | Decoded length | Loudness delta (decoded minus input) |
|---|---|---|---|
| Opus, stereo 48 kHz, 128 kb/s | own decoder | exact | -0.010 LU |
| Opus, mono, input at 8 kHz to 192 kHz, 64 kb/s | own decoder | `round(n * 48000 / rate)`, exact | worst -0.209 LU (192 kHz input); every rate up to 96 kHz within 0.14 |
| Opus, 12 to 510 kb/s ladder | own decoder | exact | worst -0.103 LU |
| AAC `.m4a`, 14 combinations of rate (7.35 to 96 kHz), channels and bitrate | own decoder with the edit list applied | exactly `n` frames, every case | worst 0.121 LU (96 kHz stereo); stereo 48 kHz, 128 kb/s +0.010 |
| AAC ADTS, the same 14 | own decoder, priming dropped | `n` frames after the priming | the same numbers (same access units) |

The 1 kHz tone keeps its level to within 1 dB in every AAC case. For AAC the 1024-sample alignment is also
confirmed with a second decoder (the importer, `soundcraft-audio-io`): the correlation between input and decoded
audio peaks at lag 0 and not at the 1024-sample lags either side. ffmpeg 6.1.1 reads every file clean
(`ffmpeg -v error -i f -f null -` prints nothing) and reports the properties the file declares. Run by hand
with `cargo test -p soundcraft-aac --features aac-encode -- --ignored` and
`cargo test -p soundcraft-opus -- --ignored`:

- ffprobe's `.m4a` duration is the input length to the microsecond; `codec_name=aac`, `profile=LC`, the
  right channel count and sample rate. For Opus it is the last granule position.
- ffmpeg skips the 1024 priming samples of an `.m4a` but still plays the tail of the last frame, so a
  decode is up to 1023 samples longer than the input. Players that honour the whole edit list stop at `n`.
- ffmpeg plays an ADTS stream whole (priming included); its duration there is an estimate from the bitrate.

### What the importer does with these files

Found while testing, outside this work's scope and noted for G-21 wiring and G-04:

- `soundcraft-audio-io` decodes `.m4a` and `.aac` with symphonia 0.5.5, which **does not apply the edit list**.
  A bounced `.m4a` re-imported into Pa Tools comes back with the 1024 priming samples at the start and up to
  1023 of padding at the end. The importer needs the gapless trim before bounce-and-reimport round-trips.
- symphonia's MP4 reader **rejects AAC at 88.2 and 96 kHz** ("unknown sample rate") although ADTS at those
  rates decodes, and ffmpeg and this crate's decoder read the `.m4a` fine.

## Building with AAC

```sh
cargo test -p soundcraft-aac --features aac-encode
cargo clippy -p soundcraft-aac --all-targets --features aac-encode -- -D warnings
cargo test -p soundcraft-aac            # the default build: the API answers Error::NotBuilt
```

Shipped `Pa Tools.app` builds use the default features, so the AAC encoder and decoder are compiled out. The
app crates do not forward `aac-encode`, so switching it on in a built app is a manifest change for whoever
makes that licensing decision. A build without the feature keeps the **same API**: `encode_m4a`, `encode_adts`
and both writers answer `Error::NotBuilt` ("AAC export is not included in this build"), `ENCODER_BUILT` is
`false`, and `bitrate_range` and `EXPORT_SAMPLE_RATES` still work for a dialog. The engine therefore needs no
`cfg`; it shows the message and offers another format. `cargo xtask ci` lints and tests `soundcraft-aac`
with `aac-encode` on as well as off, so the encoder does not rot while it is switched off.

The Cargo feature `rayon` of `soundcraft-aac` quantises channels in parallel inside the encoder. It is only
used together with `aac-encode` and does **not** switch it on: AAC encoding stays off unless `aac-encode` is
named explicitly (`cargo tree -p soundcraft-aac -e features --features rayon` shows no `aac-encode`, and no
`soundcraft-bitstream` or `soundcraft-isobmff`, among the normal dependencies).

## Patent posture

Gil's decision of 2026-10-23 (the record is kept with the G-21 plan, not in this repository). This is not
legal advice.

- AAC patent pools still license encoders and decoders. Many of the core patents have expired, but not all of
  them.
- Opus is a royalty-free codec (RFC 6716; its contributors license the essential patents royalty-free).
- The code licences (MIT OR Apache-2.0 for the donor code and for this repository) grant copyright
  permission. They do not grant patent rights for either codec.

So the Opus encoder ships enabled with no feature. The AAC encoder, decoder and writers are written, tested
and kept in the tree, but sit behind the Cargo feature `aac-encode`, which is not in `default = []`. A shipped
build compiles them out; turning the feature on is a licensing decision for whoever builds that binary, and
Gil decides when to enable it.

## Provenance

- **Code.** Six crates come from the sibling apps' clean-room code, MIT OR Apache-2.0, at pinned commits (rows
  in [`ATTRIBUTION.md`](../ATTRIBUTION.md)). The first commit of each import is verbatim apart from package
  names; hardening and adaptation are separate commits. The imported crates and what each is for here:

  | Crate | Donor | Lines imported (src) | For |
  |---|---|---|---|
  | `soundcraft-bitstream` | FilmCraft `crates/bitstream` @ `d3801e2` | 478 | Bit reader and writer for AAC |
  | `soundcraft-isobmff` | FilmCraft `crates/isobmff` @ `d3801e2` | 4 409 | MP4 muxer and demuxer (`.m4a`, edit lists) |
  | `soundcraft-aac` | FilmCraft `crates/aac` @ `d3801e2` | 5 077 | AAC-LC encoder and decoder |
  | `soundcraft-ogg` | FilmCraft `crates/ogg` @ `d3801e2` | 667 | Ogg demuxer, **test tool** (independent parser of our Opus files) |
  | `soundcraft-opus-decode` | FilmCraft `crates/opus` @ `d3801e2` | 6 309 | Opus decoder, **test tool** (round trips) |
  | `soundcraft-opus` | EffectCraft `crates/opusenc` @ `9581b53` | 5 043 | Opus encoder |

  The donor's `export/aac_out.rs` (an `.m4a` or ADTS bounce with a priming edit list) is coupled to the donor's
  project and render crates and is not imported; its design was re-implemented in `crates/aac/src/export/`,
  with an edit list that also trims the tail. The Ogg writer and the 48 kHz converter in `soundcraft-opus`
  are new, written from RFC 3533, RFC 7845 and RFC 6716 for Pa Tools.
- **Huffman tables: needs Gil's confirmation.** The AAC Huffman codebooks (`crates/aac/src/huffman_tables.rs`)
  are the normative values of ISO/IEC 14496-3 Annex 4.A. The donor authors recovered them by probing the
  behaviour of an ffmpeg binary as a black-box oracle (`crates/aac/tools/probe.py`, `derive.py`, `gen.py`
  are kept for provenance); no ffmpeg source was read or copied. The tables are standard-mandated data and are
  checked to be complete prefix codes, but the method touches a GPL-built binary, so this is flagged for the
  owner rather than assumed fine.
- **No third-party code in the Pa Tools additions.** ffmpeg and ffprobe are test tools only (`#[ignore]`d
  tests, run by hand), never a dependency.

## Hardening of the imported code

Against hostile input and the never-crash rule (`AGENTS.md`):

- No `unwrap`, `expect`, `panic!`, `assert!` or `debug_assert!` outside tests in any of the six crates;
  each crate root carries the usual `deny(clippy::unwrap_used, ...)`.
- `soundcraft-opus` constructors, `encode_float` and the transforms return `Result` or `Option` instead of
  asserting. The CELT/SILK hybrid rate control used to overshoot its frame budget (found by a random-signal
  test: full-scale white noise, mono, 16 kb/s); the SILK part now re-codes at a lower level, and finally as
  silence, when it would not leave room for the CELT part.
- Bit counts are clamped to 32, transform sizes are rounded to a power of two instead of asserted, and every
  allocation the export API makes is bounded by a chunk size, not by the input.
- The donor's one remaining debug assertion in the Opus encoder (a finished frame must fit its byte budget) is
  kept as a count, not dropped: `Stats::frame_overflows` is the number of frames whose range coder ran past the
  budget. It is 0 for every input the tests try (ordinary signals at five rates and channel layouts, the stress
  signals in every mode, the random-signal test and the proptest), and the tests assert it.
- `RangeDecoder::uint` returns 0 for `ft < 2` in debug and release alike (an empty range also sets the decoder's
  error flag), instead of asserting.

## Wiring (what the engine lane does later)

- Depend on the crates; take `soundcraft-aac` as an optional dependency behind an engine feature that
  forwards `aac-encode`, off in shipped builds.
- Add the bounce formats (Ogg Opus, M4A, ADTS) as commands with the usual params, using the streaming
  writers for progress and cancel, and pass the product name from `crates/brand` as the Opus vendor string
  and the `©too` tag.
- Decide the bounce's limiter or clipping policy (the crates clamp, and count what they clamped).
- Decide the channel layout for sessions wider than stereo (the crates take 1 or 2 channels).
- Show the `Error::NotBuilt` message and offer another format.
- Coordinate the format table with Phase 5 (`pa-formats`).
- Fix the importer's edit-list and 88.2/96 kHz `.m4a` limits above before promising bounce-and-reimport.

## Later: dedupe

`crates/isobmff` (package `soundcraft-isobmff`, imported here from FilmCraft for the `.m4a` muxer) duplicates
`crates/video/src/isobmff`: two copies of one ISO/IEC 14496-12 demuxer and muxer, about 4 500 and 4 300 lines,
that have already started to diverge (this one has `Brand::M4a`, `Mp4Writer::set_edits` for edit lists and the
memory notes above; the video copy has none of them). Keeping both is deliberate for now: the video decoders
(G-17) are a separate lane, and merging the two is a refactor of that lane's code. When both are on the main
line, make one the single implementation (the audio crate is the natural base, as it is the one with its own
package, tests and README), point `soundcraft-video` at it, port the video copy's fixes across, and delete the
other copy. Until then a fix to an MP4 box has to be made twice.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-23 | major | Created: Ogg Opus (ships) and AAC `.m4a` / ADTS (feature `aac-encode`, off in shipped builds) export crates for G-21, with limits, timing, measurements, patent posture and provenance |
| 2026-10-23 | minor | Review follow-ups: dead `video-codecs.md` link made plain text, `.m4a` writer memory stated accurately, "Later: dedupe" note for the two ISOBMFF copies, `rayon` does not imply `aac-encode`, Opus frame-overflow count |
