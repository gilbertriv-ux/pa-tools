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

## Audio codecs (ported, clean-room donor)

The compressed-audio export crates come from [FilmCraft](https://github.com/storytold/filmcraft)
(`storytold/filmcraft`) and [EffectCraft](https://github.com/storytold/effectcraft) (`storytold/effectcraft`), the
sibling apps of SoundCraft (MIT OR Apache-2.0, the same license as this repository). Each is pinned to a donor
commit, and the donor READMEs describe the code as clean-room, written from the public specifications, with
ffmpeg used only as an external test oracle. The first commit of each import is verbatim apart from package names; where a crate
differs from the donor, the last column says how. Background: `docs/audio-codecs.md`.

| Path | Donor | Written from (per the donor) | License | Local changes |
|---|---|---|---|---|
| `crates/bitstream/` | FilmCraft `crates/bitstream` @ d3801e2df8aaf9ce3c4b8b1a92d248b973dd4507 | the public specifications (bit syntax of ISO/IEC 14496) | MIT OR Apache-2.0 | package name; bit counts clamped instead of asserted |
| `crates/isobmff/` | FilmCraft `crates/isobmff` @ d3801e2df8aaf9ce3c4b8b1a92d248b973dd4507 | ISO/IEC 14496-12 (ISO base media file format) | MIT OR Apache-2.0 | package name; `Brand::M4a` and `Mp4Writer::set_edits` added; donor test kit not imported |
| `crates/aac/` | FilmCraft `crates/aac` @ d3801e2df8aaf9ce3c4b8b1a92d248b973dd4507 (the `.m4a` / ADTS design of `export/aac_out.rs` re-implemented, not copied) | ISO/IEC 14496-3 (AAC-LC). Huffman codebooks (`src/huffman_tables.rs`): the functional constants that ISO/IEC 14496-3 (MPEG-4 AAC; the same codebooks as ISO/IEC 13818-7) requires every AAC implementation to contain; all 12 checked by the independent review (sizes 121, 81×6, 64×2, 169×2 and 289, complete prefix codes, sampled codewords matching the standard); byte-identical to the donor `d3801e2`, which derived them by black-box probing a decoder (tools in `crates/aac/tools/`) without reading or copying any decoder source; the arrangement is the standard's codebook order and the formatting comes from the donor's own `gen.py`; treated as uncopyrightable facts (17 U.S.C. § 102(b)), not legal advice; accepted by Gil on 2026-10-23 | MIT OR Apache-2.0 | package name; everything behind the Cargo feature `aac-encode` (off by default, off in shipped builds); validated export API (`src/export/`); no-panic hardening |
| `crates/ogg/` | FilmCraft `crates/ogg` @ d3801e2df8aaf9ce3c4b8b1a92d248b973dd4507 | RFC 3533, RFC 7845 | MIT OR Apache-2.0 | package name; used only as a test tool (independent parser of the Opus files) |
| `crates/opus-decode/` | FilmCraft `crates/opus` @ d3801e2df8aaf9ce3c4b8b1a92d248b973dd4507 | RFC 6716 | MIT OR Apache-2.0 | package name; no-panic hardening; used only as a test tool (round-trip decoder) |
| `crates/opus/` (`src/lib.rs`, `celt`, `silk` and friends) | EffectCraft `crates/opusenc` @ 9581b53c3f8945683252af0eb5fac79e0e43aed4 | RFC 6716 | MIT OR Apache-2.0 | package name; fallible constructors and `Result` API, SILK rate-control fallback, no-panic hardening |
| `crates/opus/src/{ogg,rate_convert,stream,error}.rs` | Pa Tools (GR8 ID YUZ.AI), new | RFC 3533 (Ogg pages, CRC-32), RFC 7845 (Opus in Ogg), a Kaiser-windowed sinc resampler | MIT OR Apache-2.0 | n/a (not donor code) |

**AAC Huffman codebooks.** `crates/aac/src/huffman_tables.rs` holds the codebooks that ISO/IEC 14496-3 requires of
every AAC implementation (see the `crates/aac/` row for the checks, the donor identity and the basis on which
they are treated as uncopyrightable facts). The donor authors recovered them by probing a decoder's output as a
black-box oracle (the scripts are kept in `crates/aac/tools/`); no decoder source code was read or copied. Accepted
by Gil on 2026-10-23; it does not change the licence column. Not legal advice.

## Fonts

Pa Tools bundles no font files. It uses the default fonts shipped inside the `egui` crate
(Ubuntu-Light: Ubuntu Font Licence 1.0; Hack: MIT; Noto Emoji: OFL-1.1; emoji-icon-font: OFL-1.1/MIT)
and, at runtime, the operating system's own UI fonts when available (never redistributed).
The title in the macOS DMG background (`packaging/macos/dmg/background.svg`) is text from Inter
(SIL OFL 1.1) converted to outlines; no font file is shipped.

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
