# Attribution

Every asset bundled with or committed to Pa Tools is listed here with its author, source and
license. `cargo xtask assets` fails CI when a tracked asset file has no row.

Pa Tools is based on [SoundCraft](https://github.com/storytold/soundcraft) by the ArtCraft team and
the SoundCraft contributors (MIT OR Apache-2.0); see `NOTICE` and `UPSTREAM.md`. Rows credited to
the SoundCraft contributors are upstream's.

**Rule:** no Avid, Pro Tools, Adobe or other proprietary icons, images, sounds or presets — ever.
Assets must be original, public domain (CC0) or permissively licensed. See `AGENTS.md`.

## Icons

All UI icons (tools, transport, track buttons) are drawn procedurally in
`crates/ui-egui/src/icons.rs` by the SoundCraft contributors (MIT OR Apache-2.0). No UI icon image
files are used. The app icon (`assets/app-icon/`, a gold "Pa" wordmark over a waveform on black,
drawn as original geometric SVG for Pa Tools) is listed under Files below.

## Audio

The demo session's audio (drums, bass, pad) and its MIDI parts are synthesised in code in
`crates/engine/src/demo.rs` by the SoundCraft contributors (MIT OR Apache-2.0). No sample files
are bundled.

## Fonts

Pa Tools bundles no font files. It uses the default fonts shipped inside the `egui` crate
(Ubuntu-Light: Ubuntu Font Licence 1.0; Hack: MIT; Noto Emoji: OFL-1.1; emoji-icon-font: OFL-1.1/MIT)
and, at runtime, the operating system's own UI fonts when available (never redistributed).
The title in the macOS DMG background (`packaging/macos/dmg/background.svg`) is text from Inter
(SIL OFL 1.1) converted to outlines; no font file is shipped.

## Video codecs (ported, clean-room donor)

The decoders in `crates/video` come from [FilmCraft](https://github.com/storytold/filmcraft) (`storytold/filmcraft`,
the sibling app of SoundCraft; MIT OR Apache-2.0, the same license as this repository). Each is pinned
to a donor commit, and the donor states that it wrote them from the public specifications. Where a
module differs from the donor, the last column says how.

| Path | Donor | Written from (per the donor) | License | Local changes |
|---|---|---|---|---|
| `crates/video/src/vc3/` | FilmCraft `filmcraft-dnx` @ f47a1f6 | SMPTE ST 2019-1:2016 and Amendment 1:2023 | MIT OR Apache-2.0 | hardening, neutral names |
| `crates/video/src/hevc/` | FilmCraft `crates/hevc` @ f47a1f6 | ITU-T Rec. H.265 (ISO/IEC 23008-2) | MIT OR Apache-2.0 | hardening, neutral names |
| `crates/video/src/av1/` | FilmCraft `crates/av1` @ f47a1f6 | AV1 Bitstream & Decoding Process Specification v1.0.0 with Errata 1 | MIT OR Apache-2.0 | hardening and adaptation |
| `crates/video/src/bitstream.rs`, `isobmff/`, `h264/`, `prores/` | FilmCraft `filmcraft-bitstream`, `-isobmff`, `-h264`, `-prores` (`bitstream.rs` matches f47a1f6 apart from checked arithmetic in `length_prefixed_nals`; the others are an earlier copy, commit not recorded) | the public specifications | MIT OR Apache-2.0 | hardening (checked arithmetic, bounded allocations), module paths |

The HEVC and AV1 decoders are Cargo features that are off by default; see `docs/video-codecs.md`.

## Video test fixtures

Small test samples for the video decoders, in `crates/video/tests/fixtures/`. They are Gil's decision of
2026-10-23 (G-17); this is not legal advice.

| Path | Provenance |
|---|---|
| `crates/video/tests/fixtures/vc3/dnxhr-lb-256x144-2f.mov` | VC-3 regression sample generated with the FFmpeg 6.1.1 native dnxhd encoder (libavcodec 60.31.102; dnxhd source LGPL-2.1-or-later; the FFmpeg build is --enable-gpl, so the binary is GPL-2.0-or-later) from a synthetic test pattern; the sample is encoder output, not a derivative work of the encoder, and no GPL code is included in this repository. |
| `crates/video/tests/fixtures/hevc/hevc-main-8bit-64x64-4f.h265`, `hevc-main-8bit-64x64-4f.mp4`, `hevc-main-8bit-64x64-bframes2-ref2-48f.mp4`, `hevc-main10-64x64-4f.mp4` | HEVC regression samples generated with libx265 3.5+1-f0c1022b6 (GPL-2.0-or-later) from synthetic test patterns; the samples are encoder output, not derivative works of the encoder, and no GPL code is included in this repository. |
| `crates/video/tests/fixtures/hevc/hevc-main-8bit-64x64-opengop-32f.mp4` | HEVC open-GOP regression sample (IDR then CRA pictures with RASL leading pictures) generated with libx265 3.5+1-f0c1022b6 (GPL-2.0-or-later), options `open-gop=1:keyint=8:min-keyint=8:bframes=2`, from a synthetic test pattern; the sample is encoder output, not a derivative work of the encoder, and no GPL code is included in this repository. |
| `crates/video/tests/fixtures/av1/av1-8bit-64x64-4f.ivf`, `av1-8bit-64x64-4f.mp4`, `av1-10bit-64x64-4f.ivf`, `av1-10bit-64x64-4f.mp4` | AV1 regression samples generated with libaom v3.8.2 (BSD-2-Clause, with the Alliance for Open Media Patent License 1.0) from synthetic test patterns; the samples are encoder output, not derivative works of the encoder, and no GPL code is included in this repository. |
| `crates/video/tests/fixtures/av1/regress-grain-points.frames`, `regress-scaled-reference.frames` | libaom-encoded synthetic test pictures with a few header bits altered, so they work as fuzz regression inputs. Same licence note as the AV1 regression samples above. |

## Third-party code and workflow provenance

Dependencies and adapted files that are not assets (so `cargo xtask assets` does not check them),
recorded here for their licence and provenance.

| Path | Author | Source | License |
|---|---|---|---|
| `symphonia` 0.5.5 and its `symphonia-*` crates (16 packages in `Cargo.lock`; decode MP3, FLAC, Ogg Vorbis, WAV, AIFF, ALAC, AAC, MP4, CAF and MKV for `crates/audio-io`) | The Project Symphonia Developers (Philip Deljanov and contributors) | https://github.com/pdeljanov/Symphonia, used **unmodified** as released on crates.io (no `[patch]`, no vendored copy); the notice is in `NOTICE` | MPL-2.0, file-level copyleft (https://mozilla.org/MPL/2.0/). Unmodified MPL-2.0 dependencies are allowed (decision 2026-10-23); modifying MPL-2.0 files needs a new decision. |
| `.github/workflows/ci.yml` | Pa Tools (GR8 ID YUZ.AI), adapted from the CI workflows of FilmCraft and PhotoCraft (ArtCraft Team and the FilmCraft / PhotoCraft contributors) | https://github.com/storytold/filmcraft (`.github/workflows/ci.yml`, the quality-gate job) and https://github.com/storytold/photocraft (`.github/workflows/ci.yml`, the OS matrix) | MIT OR Apache-2.0 |
| `.github/workflows/windows-msi.yml`, `.github/scripts/ci-digest.sh` | Pa Tools (GR8 ID YUZ.AI) | original; the workflow only calls the repository's own `packaging/windows/` scripts. Nothing is taken from storytold/craft-common-actions, which has no licence file | MIT OR Apache-2.0 |

## Files

| Path | Author | Source | License |
|---|---|---|---|
| `assets/app-icon/icon.svg` | Pa Tools (GR8 ID YUZ.AI) | original (geometric paths generated by `packaging/icon_svg.py`: a "Pa" wordmark from rings and bars, a waveform of rounded bars and stroke-built "TOOLS" lettering; no existing logo or font used) | MIT OR Apache-2.0 |
| `packaging/macos/dmg/` (all files) | @XusBadia (layout, generator); Pa Tools (GR8 ID YUZ.AI) (Pa Tools background artwork and its outlined Inter title) | original (macOS DMG window background and layout, from the app icon; see `packaging/macos/dmg/README.md`) | MIT OR Apache-2.0 (Inter outlines: SIL OFL 1.1) |
| `assets/app-icon/patools-1024.png` | Pa Tools (GR8 ID YUZ.AI) | original, rendered from `icon.svg` | MIT OR Apache-2.0 |
| `assets/app-icon/patools-macos-512.png` | Pa Tools (GR8 ID YUZ.AI) | original, rendered from `icon.svg` | MIT OR Apache-2.0 |
| `assets/app-icon/Pa Tools.icns` | Pa Tools (GR8 ID YUZ.AI) | original, packed from the iconset below by `packaging/icons.py` (regenerate with `iconutil` on a Mac) | MIT OR Apache-2.0 |
| `assets/app-icon/Pa Tools.iconset/` | Pa Tools (GR8 ID YUZ.AI) | original, rendered from `icon.svg` (the ten PNGs `iconutil` reads) | MIT OR Apache-2.0 |
| `assets/app-icon/patools.ico` | Pa Tools (GR8 ID YUZ.AI) | original, rendered from `icon.svg` | MIT OR Apache-2.0 |
| `assets/app-icon/hicolor/` | Pa Tools (GR8 ID YUZ.AI) | original, rendered from `icon.svg` (16–512 px PNGs and the scalable SVG) | MIT OR Apache-2.0 |
| `docs/images/` | SoundCraft contributors | screenshots of SoundCraft (upstream UI, before the Pa Tools rebrand; to be replaced with Pa Tools screenshots) rendered from the synthesised demo session | MIT OR Apache-2.0 |

| `docs/images-patools/edit.jpg`, `docs/images-patools/mix.jpg`, `docs/images-patools/about.jpg` | Pa Tools screenshots captured on 2026-10-22 from the V2 Intel build (demo song synthesised by the app) | Pa Tools build | MIT OR Apache-2.0 |
