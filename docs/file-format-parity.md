# File-format parity

> **Last reviewed:** 2026-10-23 · **Last updated:** 2026-10-23 · **Change:** minor (video: VC-3 decode; HEVC and AV1 decode behind compile-time features; previously trivial: WAV empty data chunk with trailing metadata (#466)) · **Target:** Avid Pro Tools Ultimate 2026.4.1

Every format Pro Tools reads or writes, and ours. The Pro Tools list comes from the document types
its bundle declares (`CFBundleDocumentTypes` in Info.plist: names only, nothing else read) and its
documented import/bounce options. Ours comes from `crates/audio-io` (`detect_format`, `decode`,
`ENCODE_EXTENSIONS`), `crates/engine/src/cmd/file.rs`, `crates/engine/src/score.rs`,
`crates/engine/src/clip_group_file.rs` and `crates/video`.

**File formats: ~40 % ready** (estimated: weighted by how often a Pro Tools user hits each row;
session interchange carries a third of the weight and is at 0 %). Remaining ≈ 70–120 h.

Legend: ✅ supported and tested · 🟡 partial · ❌ missing · — not applicable.

## Sessions and interchange

| Format | Pro Tools | Read | Write | Tests / notes |
|---|---|---|---|---|
| Pro Tools session `.ptx` (and `.ptf` from 7–9) | native | ❌ | ❌ | Proprietary, undocumented. Owner decision needed on black-box format study (G-03) |
| Session templates `.ptxt` | read/write | ❌ | ❌ | We have no templates at all (G-16) |
| AAF (Advanced Authoring Format) | import/export | ❌ | ❌ | #40. The post-production handoff from picture editors (G-03) |
| OMF | import/export | ❌ | ❌ | Legacy, still delivered by some editors |
| Media Composer–compatible session | export | ❌ | ❌ | AAF variant |
| ADM BWF (Dolby Atmos master) | import/export | ❌ | ❌ | G-12 |
| Pa Tools session `.scraft` + `Audio Files/` | — | ✅ | ✅ | JSON (serde); mutated-file property test loads or fails cleanly; plugin state embedded. Fades save optional `curvature` (-1..1); missing values default to 0 and preserve legacy envelopes |
| Import Session Data (from another session) | Pro Tools sessions | 🟡 | — | From `.scraft` only |
| Session info as text | export | — | ✅ | `file.export_session_text` |
| Clip groups `.rgrp` / ours `.scgrp` | read/write | 🟡 | 🟡 | Our own `.scgrp` format; Pro Tools' clip-group files not read |
| Track presets, fade presets, I/O settings, workspace, sync settings | read/write | ❌ | ❌ | Their equivalents don't exist yet (G-16) |

## Audio

| Format | Pro Tools | Read | Write | Tests / notes |
|---|---|---|---|---|
| WAV / BWF (PCM 16/24/32, float 32), `bext` | native | ✅ | ✅ | Native reader/writer, TPDF dither option; also 8-bit and float 64 read. An empty data chunk followed by metadata reads as no audio; an unfinalised recording's audio is still read (#466). |
| RF64 / BW64 (> 4 GiB) | read/write | ✅ | ✅ | `encode_wav_rf64` |
| WAV `WAVE_FORMAT_EXTENSIBLE` multichannel with channel mask | read/write | ✅ | ✅ | `encode_with_channel_mask`; default 5.1 import creates a stereo track (#89, PR #128) |
| AIFF / AIFF-C | native | ✅ | ✅ (`fl32`) | Native |
| FLAC | import | ✅ | ✅ | Own encoder |
| MP3 | import, bounce | ✅ (symphonia) | ❌ | G-21 |
| AAC / M4A / MP4 audio | import, bounce | ✅ (symphonia) | ❌ | G-21 |
| ALAC | import | ✅ | ❌ | |
| Ogg Vorbis | — | ✅ | ❌ | Beyond Pro Tools |
| CAF | — | ✅ | ❌ | |
| Sound Designer / SD II | import (legacy) | ❌ | — | Rare today |
| ReCycle / REX | import | ❌ | — | Loops |
| MXF audio (OP-Atom, OP1a) | import | ❌ | ❌ | Avid media; FilmCraft has an MXF reader to learn from |
| iXML / polyphonic field-recorder metadata | read | ❌ | ❌ | G-17 |
| Peak overviews | `.wfm` cache | ✅ (own, in memory) | — | `audio-io::peaks`; not cached on disk |

## MIDI and notation

| Format | Pro Tools | Read | Write | Tests / notes |
|---|---|---|---|---|
| Standard MIDI File (type 0/1) | import/export | ✅ | 🟡 | Exported MIDI omits imported controllers and ignores the tracks filter (#87, #98; PR #125) |
| MusicXML (to Sibelius) | export | — | ✅ | `file.export_sibelius`, `score.rs` |
| Printable score | print | — | ✅ (SVG) | |

## Video

| Format | Pro Tools | Read | Write | Tests / notes |
|---|---|---|---|---|
| QuickTime / MP4, H.264 | import, bounce | ✅ | ❌ | Own pure-Rust decoder (Baseline/Main/High); mutated-movie fuzzing |
| ProRes (all flavours) | import | ✅ | ❌ | Own decoder (shown as "PA ProRez") |
| Motion JPEG | import | ✅ | ❌ | |
| VC-3 (DNxHD / DNxHR) in QuickTime MOV | import | ✅ | ❌ | Own pure-Rust decoder, always built: every compression ID (DNxHD 1235-1260, DNxHR LB/SQ/HQ/HQX/444), 8/10/12-bit, 4:2:0/4:2:2/4:4:4/RGB, interlaced; matches a reference decode of a committed sample to within two levels per sample (not bit-exact: the reference uses an integer inverse DCT); shown as "VC-3" with its variant |
| HEVC (H.265) Main / Main 10 in MP4 / MOV | import | 🟡 | ❌ | Own decoder behind the Cargo feature `hevc-decode`: ✅ in builds with it, **off in shipped builds** for patent-licensing reasons (Gil's decision, [video-codecs.md](video-codecs.md)). 4:2:0 only; 4:2:2, 4:4:4 and range-extension files open but say they are unsupported. Bit-exact against reference decodes of committed samples |
| AV1 in MP4 / MOV | import | 🟡 | ❌ | Own decoder behind the Cargo feature `av1-decode`: ✅ in builds with it, **off in shipped builds** for patent-licensing reasons (same decision). Every profile, 8/10/12-bit, 4:2:0/4:2:2/4:4:4/monochrome. Bit-exact against reference decodes of committed samples |
| MXF video, Matroska / WebM, IVF files | import | ❌ | ❌ | G-17: no reader for these containers, so VC-3, HEVC and AV1 are read from MP4 / MOV only |
| Bounce with video | export | — | ❌ | G-17 |

HDR pictures are shown without tone mapping, and VC-3 alpha is not used (pictures are opaque).
Pictures are limited to 8192 × 8192 pixels (67 million) and each decoder to a 1 GiB working budget; these
are defaults, not a recorded decision. The 🟡 on HEVC and AV1 is only the build switch: the code is done and tested
(CI runs it with both features on). The ~40 % file-formats estimate above does not move.

## Plugin and preset files

| Format | Pro Tools | Ours |
|---|---|---|
| AAX plug-ins | the only format it hosts | ❌ (cannot: Avid SDK licence, PACE signing) |
| VST3 / Audio Units / CLAP | — (AAX only) | ✅ hosted (ahead of Pro Tools) |
| Plug-in settings `.tfx` | read/write | ❌ (our own JSON presets per plugin) |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-23 | minor | Video (G-17): VC-3 import ✅ in MOV; HEVC and AV1 import written and tested but behind the compile-time features `hevc-decode` / `av1-decode`, off in shipped builds (patent posture, [video-codecs.md](video-codecs.md)) so shown 🟡; MXF / Matroska / WebM / IVF readers still missing. Readiness estimate unchanged |
| 2026-10-13 | trivial | WAV: empty data chunk with trailing metadata (#466) |
| 2026-10-13 | minor | Fade curvature round-trips in sessions with a zero default for older files. Format version and readiness estimate unchanged |
| 2026-10-10 | major | Created: format inventory against Pro Tools 2026.4.1's declared document types and import/bounce options |
