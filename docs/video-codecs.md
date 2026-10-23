# Video codecs

> **Last reviewed:** 2026-10-23 · **Last updated:** 2026-10-23 · **Change:** major (created with the G-17 video decoders)

What `crates/video` (package `soundcraft-video`) can decode, which decoders ship, and why two of them
are compiled out of shipped builds. For the format table see [`file-format-parity.md`](file-format-parity.md);
for what is still missing see G-17 in [`gaps.md`](gaps.md).

## What decodes

| Codec | Build | Notes |
|---|---|---|
| H.264 (Baseline, Main, High) | always | Progressive 8-bit 4:2:0, CAVLC and CABAC |
| ProRes (422 Proxy to 4444 XQ) | always | Shown on screen as "PA ProRez" |
| VC-3 (DNxHD / DNxHR) | always | Every compression ID, 8/10/12-bit, 4:2:0 / 4:2:2 / 4:4:4 / RGB, interlaced; shown as "VC-3" plus the variant, e.g. "VC-3 (SQ, 8-bit)" |
| Motion JPEG | always | `jpeg`, `mjpa` |
| HEVC / H.265 | Cargo feature `hevc-decode` | Main and Main 10, 4:2:0, 8 to 12 bits. 4:0:0, 4:2:2, 4:4:4 and range-extension files open but say they are unsupported |
| AV1 | Cargo feature `av1-decode` | Main, High and Professional, 8/10/12-bit, 4:2:0 / 4:2:2 / 4:4:4 / monochrome |

All of them are read from MP4 and QuickTime MOV. MXF, Matroska / WebM and IVF files are not read yet
(G-17). HDR pictures are converted with the stream's matrix (BT.601, 709 or 2020) and are not tone mapped.

A build without `hevc-decode` or `av1-decode` still opens such a file, so the clip shows on the timeline
with its codec named, and its pictures report "unsupported in this build". `MovieInfo.codec` ids are
`h264`, `prores`, `dnxhd`, `hevc`, `av1` and so on; the ids did not change when the on-screen labels did.

## Building with the optional decoders

```sh
cargo test -p soundcraft-video --features hevc-decode,av1-decode
cargo clippy -p soundcraft-video --all-targets --features hevc-decode,av1-decode -- -D warnings
```

Shipped `Pa Tools.app` builds use the default features, so both decoders are compiled out. The app crates
do not forward the two features, so switching them on in a built app is a manifest change for whoever
makes that licensing decision. `cargo xtask ci` lints and tests the video crate with `hevc-decode` and with
`hevc-decode,av1-decode`, so neither decoder rots while it is switched off.

## Limits (defaults, not a recorded decision)

- Pictures up to 8192 × 8192 pixels (`MAX_PIXELS`, 67 million); larger sizes are refused before anything is allocated.
- A working-memory budget of 1 GiB per decoder: the HEVC decoded-picture buffer (`MAX_DPB_BYTES`), the AV1
  reference frames and working buffers (`DEFAULT_MEMORY_BUDGET`, adjustable with `Decoder::set_memory_budget`)
  and a VC-3 frame (`MAX_DECODE_BYTES`).
- The `threads` feature (rayon) is unchanged and off by default; the decoders run one thread each inside `Movie`.

The size and budget were open question 5 in the G-17 plan and have no recorded answer from Gil; they are
the values the lanes shipped with.

## Patent posture

Gil's decision of 2026-10-23 (decision 4 in `V1-DECISIONS-G17-2026-10-23.md`, kept with the G-17 plan, not in
this repository). This is not legal advice.

- HEVC patent pools charge royalties.
- Patent pools that are not members of the Alliance for Open Media assert claims against AV1.
- The code licences (MIT OR Apache-2.0 for the FilmCraft donor code and for this repository) grant
  copyright permission. They do not grant patent rights for either codec.

So the HEVC and AV1 decoders are written, tested and kept in the tree, but sit behind the Cargo features
`hevc-decode` and `av1-decode`, which are not in `default = []`. A shipped build compiles them out;
turning one on is a licensing decision for whoever builds that binary. VC-3 (DNxHD / DNxHR) has no feature
and ships enabled.

## Provenance

- **Code.** `crates/video/src/vc3/`, `hevc/` and `av1/` come from FilmCraft (`storytold/filmcraft`) at
  commit `f47a1f6`, MIT OR Apache-2.0, with hardening against hostile files and neutral names or adaptation
  as the local changes. The VC-3 decoder was written from SMPTE ST 2019-1:2016 and Amendment 1:2023. Rows are
  in [`ATTRIBUTION.md`](../ATTRIBUTION.md).
- **Test samples.** Tiny synthetic test patterns under `crates/video/tests/fixtures/`, made outside the
  repository with an encoder per codec. Each has its own row in `ATTRIBUTION.md` (Gil's wording of 2026-10-23;
  encoder output, no encoder code in the repository). The HEVC and AV1 decode tests compare against SHA-256
  hashes of an independent decoder's output, so CI needs no external tool.

## On-screen names

No third-party product name appears in the app (decision 1): ProRes is shown as "PA ProRez", and the
DNx family by its SMPTE standard name, "VC-3". A test in `crates/video/src/seam_tests.rs` asserts that no
label or error message contains Apple, ProRes, Avid or DNx. These documents may name formats and encoders.

## Loose ends

`crates/engine/src/cmd/file.rs` (the `file.import_video` doc string) still lists "ProRes"; it waits for G-04
and is not part of the video crate.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-23 | major | Created: decoders by build, limits, patent posture and provenance for the G-17 VC-3, HEVC and AV1 decoders |
