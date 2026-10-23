# Video codecs

> **Last reviewed:** 2026-10-23 · **Last updated:** 2026-10-23 · **Change:** minor (AV1 decoder budget 1.6 GiB: full 8K UHD at 4:2:0; one frame cache for the whole app; decisions V4 5b and 6)

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
| HEVC / H.265 | Cargo feature `hevc-decode` | Main and Main 10, 4:2:0, 8 to 12 bits, closed and open GOPs. A stream whose luma and chroma depths differ (say 8 and 10) is decoded and shown at the deeper depth; only synthetic streams cover that case, because no encoder makes one and the reference decoder refuses it. 4:0:0, 4:2:2, 4:4:4 and range-extension files open but say they are unsupported |
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
makes that licensing decision. `cargo xtask ci` lints and tests the video crate with `hevc-decode`, with
`hevc-decode,av1-decode` and with `hevc-decode,av1-decode,threads`, so neither decoder (nor the parallel
path) rots while it is switched off.

## Limits

Gil, decisions 5, 5a, 5b and 6 (V4), 2026-10-23 (the record is `V4-DECISIONS-G17-2026-10-23.md`, with V1 to
V3 beside it, kept with the G-17 plan, not in this repository). Every number below is marked **computed**
(from the formula the code uses, and pinned by a test) or **measured** (a counting allocator, or the
process's peak resident memory, while a real stream decoded). The measuring was done with the decoder's own
formula first and a run at the largest admitted size second.

There are three ceilings, plus the picture size limit they all sit under. They add up; none of them is
another's total.

| Limit | Value | What it bounds | Where |
|---|---|---|---|
| Picture size | 8192 × 8192 samples (67,108,864), every codec | One picture, checked before anything is allocated | `MAX_PIXELS` |
| **Ceiling 1.** Decoder working memory | AV1 **1.6 GiB**; HEVC 1 GiB; VC-3 1 GiB, **per decoder** | What one decoder holds to decode: reference frames, frames in flight, its state. **Not** the cache below | `DEFAULT_MEMORY_BUDGET`, `MAX_DPB_BYTES`, `MAX_DECODE_BYTES` |
| **Ceiling 2.** Frame cache | **1 GiB for the whole app**, shared by every open movie, every codec | Decoded RGBA frames kept for scrubbing and replay, counted in bytes, least recently used out first whichever movie they belong to | `DEFAULT_CACHE_BYTES`, `FrameCache` |
| **Ceiling 3.** Combined peak | Decoders of the movies decoding at once + 1 GiB + thumbnails (see below) | What the app can hold at its worst | not a constant |
| Hold-over and thumbnails | 256 MiB each, per movie | Pictures a running decoder has produced for requests that have not come yet (`ready`, at most 32 pictures); timeline thumbnails | `READY_MAX_BYTES`, `THUMB_CACHE_BYTES` |

The decoder budget and the cache are different things. The decoder budget is working memory that a hostile
or merely huge stream could otherwise make the decoder allocate; the cache is memory the app chooses to
spend on keeping finished frames. **The "1.6 GiB" of AV1 is the decoder alone**, and it is per decoder: each
open movie has its own. **The 1 GiB of cache is the app's, not a movie's** (decision 6; it used to be per
movie, so eight open movies could hold eight times it). One open AV1 movie can use the decoder budget plus
its share of the cache plus the hold-over (see "Combined peak").

### Which limit an 8K file hits first

| File | What happens | Limit hit first |
|---|---|---|
| AV1 7680 × 4320, 4:2:0, 8 or 10 bits | Decodes | None. It needs 1.594 GiB (1,711,365,120 bytes) of the 1.6 GiB budget (computed); the peak measured at that size is 1.13 GiB |
| AV1 7680 × 4320 with film grain, 4:2:2, 4:4:4 or both | Refused when the file opens: "AV1 7680x4320 pictures are unsupported in this build: picture too large for the memory budget" | Decoder working memory: needs 1.996, 2.022, 2.879 and 3.280 GiB (4:2:0 + grain, 4:2:2, 4:4:4, 4:4:4 + grain; computed) |
| AV1 8192 × 4320 or 8192 × 4096 (DCI 8K), 4:2:0 | Refused the same way | Decoder working memory: needs 1.698 and 1.604 GiB (computed); the DCI picture is 3.8 MB over |
| AV1 8192 × 8192 | Refused when the file opens: "AV1 8192x8192 pictures are unsupported in this build: picture size beyond the decoder's limits" | AV1's own level 6.3 area limit, 35,651,584 samples (8192 × 4352 at most), before any memory is counted |
| HEVC 7680 × 4320, 8-bit | Decodes | None. The cache fills after 8 frames, then the oldest go |
| HEVC 7680 × 4320, 10 or 12 bits | Decodes if the stream declares a decoded-picture buffer of 8 pictures or fewer, else refused ("needs more memory than the decoder may use") | Decoder working memory, only for large buffers |
| HEVC, anything not 4:2:0 | Opens, says unsupported | Not a memory limit |
| VC-3 up to 8192 × 8192, any layout | Decodes | None. The cache fills after 8 frames at 8K UHD |
| Any codec over 8192 × 8192 samples | Refused | Picture size |

### Ceiling 1: decoder working memory

**AV1: 1.6 GiB (1,717,986,918 bytes, `1.6 × 2^30` rounded down).** Gil, decision 5b (V4), 2026-10-23: it was
1.5 GiB, 96 MiB short of full 8K UHD. `Decoder::set_memory_budget` changes it. A frame is admitted when
eight reference slots plus one frame being decoded fit. The decoder's estimate (`memory_needed`, from the
same `frame_costs` it checks with) counts `u16` samples whatever the bit depth, so 8-bit, 10-bit and 12-bit
pictures of one layout cost the same.

| Layout | Largest 16:9 picture admitted (width in steps of 16) | Needs for 7680 × 4320 | Peak resident memory measured at the largest picture, one thread |
|---|---|---|---|
| 4:2:0, 8-bit | **7680 × 4320** (full 8K UHD; 7680 × 4352 also fits) | 1.594 GiB | 1.09 GiB at 7680 × 4320 (1.13 GiB counted by an allocator) |
| 4:2:0, 10-bit | **7680 × 4320** (same: bit depth costs nothing extra) | 1.594 GiB | 1.09 GiB at 7680 × 4320 |
| 4:4:4, 12-bit, with film grain | **5232 × 2943** | 3.280 GiB | 1.09 GiB at 5232 × 2943 |
| 4:2:0, 10-bit, with film grain | 6816 × 3834 | 1.996 GiB | 0.99 GiB at 6816 × 3834 |
| 4:2:2, 10-bit | 6784 × 3816 | 2.022 GiB | 1.13 GiB at 6784 × 3816 |
| 4:4:4, no grain | 5632 × 3168 | 2.879 GiB | 1.08 GiB at 5632 × 3168 |
| Monochrome | 7952 × 4473 (the level 6.3 area, 35,651,584 samples, stops it before the budget) | 1.166 GiB | 0.80 GiB at 7952 × 4473 |

At 1.5 GiB these were 7424 × 4176 (4:2:0), 5056 × 2844 (4:4:4 + grain), 6592 × 3708, 6528 × 3672 and 5456 × 3069.
The limits are computed. The last column is the process's peak resident memory (`VmHWM`) with eight distinct
reference frames in place, each layout in a process of its own so that memory an earlier measurement freed
cannot sit resident and make a later one read low (`cargo test -p soundcraft-video --features av1-decode
--test av1_peak -- --ignored --nocapture`, which also asserts the peak is under the estimate and under the
budget). Resident memory counts touched pages only, so it is a lower bound on what a counting allocator
sees; the allocator figure for 8K UHD 4:2:0 is 1.13 GiB. The estimate is deliberately pessimistic: the
resident peak is 62 to 71 percent of it at these sizes (68 percent for 8K UHD 4:2:0; 71 percent by the
allocator's figure).

**What stays refused**, with the same clear message when the file opens: 8K UHD with film grain (1.996 GiB),
at 4:2:2 (2.022 GiB) and at 4:4:4 (2.879 GiB), and 8192 × 4320 or 8192 × 4096 in any layout. The picture size
ceiling (8192 × 8192) and AV1's own level 6.3 area limit are unchanged.

**The refusal at open reads the declared maximum size.** `adapt_av1::describe` checks the sequence header's
declared maximum frame size (`max_frame_width` × `max_frame_height`, from the `av1C` record), because it runs
when the file is opened, before a frame is read, and the refusal has to come before anything is allocated.
So a stream that declares an 8K ceiling but codes smaller frames is refused when it opens, with the message
for the declared size; a stream that declares a smaller ceiling is admitted, whatever its encoder meant. The
decoder's own check, at each frame header, uses the coded frame size. Encoders write the ceiling they mean to
use, so this is the size the file will need.

With the `threads` feature and several threads the decoder may decode frames in parallel, and the measured
peak rises: 1.22 GiB for 4:2:0 and up to 1.54 GiB for 4:2:0 with film grain at 6592 × 3708 on four threads
(measured at the 1.5 GiB budget, about 3 percent above it). The budget is a guard against sizes a header
asks for, not a hard cap on the process. `Movie` decodes AV1 on one thread, so the app does not meet that.

**HEVC: 1 GiB for the decoded-picture buffer** (`MAX_DPB_BYTES`). A picture costs 1.578 bytes per pixel at 8
bits and 3.078 at 9 to 12 bits (the samples plus the motion field kept for prediction), and the buffer is
the declared size plus two pictures. So the largest declared buffer (`max_dec_pic_buffering`, 16 at most)
that fits is, **computed**:

| Picture | 4:2:0, 8-bit | 4:2:0, 10 or 12 bits | 4:4:4, 12-bit, grain |
|---|---|---|---|
| 1920 × 1080, 3840 × 2160, 5120 × 2880 | 16 (any) | 16 (any) | not decoded |
| 7680 × 4320 | 16 (any) | 8 | not decoded |
| 8192 × 4320 | 16 (any) | 7 | not decoded |
| 8192 × 8192 | 8 | 3 | not decoded |

HEVC here is 4:2:0 only, and has no film grain stage. The decoder tolerates streams that understate their
buffer by up to two pictures, only while they fit the 1 GiB. The independent review measured about 625 MB
for HEVC at 8K, well inside the budget.

**VC-3: 1 GiB for one decode** (`MAX_DECODE_BYTES`). It is intra-only, so there is no buffer of references,
only the planes of the picture being decoded. At 8192 × 8192, **computed**: 4:2:0 403 MB, 4:2:2 537 MB,
4:4:4 805 MB, 4:4:4 with alpha exactly 1 GiB. Every picture the size limit lets through fits, whatever the
bit depth (4:4:4 12-bit: 805 MB). The independent review measured 805 MB for the 4:4:4 case.

### Ceiling 2: the shared frame cache

**One cache for the whole app: 1 GiB (1,073,741,824 bytes) of decoded RGBA frames, shared by every open
movie** (Gil, V4 decision 6, 2026-10-23). It was one 1 GiB cache per movie, and the Video track keeps up to
eight movies open, so memory could grow eight-fold. The Video track's worker (`Worker` in
`crates/ui-egui/src/video_track.rs`) makes one `FrameCache`, and opens every movie with a clone of its handle
(`Movie::open_source_with_cache`); a movie opened any other way (`Movie::open`, `Movie::open_source`) has a
private cache of the same 1 GiB. Four bytes a pixel, so the cache holds, **computed** (and pinned by a test),
this many frames in all, whichever movies they belong to:

| Picture | One RGBA frame | Frames held |
|---|---|---|
| 1920 × 1080 | 8.3 MB | 129 |
| 3840 × 2160 (4K UHD) | 33.2 MB | 32 |
| 7680 × 4320 (8K UHD) | 132.7 MB | 8 (just: 1,062 MB of 1,074 MB) |
| 8192 × 8192 | 268.4 MB | 4 |

How it behaves (`crates/video/src/cache.rs`, tested in `seam_tests.rs`):

- **Counted in bytes, exactly.** The total of every frame's `rgba` length never exceeds the budget. A frame
  is inserted, then the least recently used frames go until the total is back inside it. **A frame larger
  than the whole budget is decoded and returned but not cached.** (Before, the newest frame was always
  kept, so such a picture was cached alone.)
- **Global least recently used.** A hit makes a frame the newest. When the cache is full, the oldest frame
  goes, whichever movie it belongs to. `FrameCache::cached_bytes` is the app-wide total (`Movie::cached_bytes`
  is one movie's share).
- **Plain LRU, no per-movie reserve.** A movie that is playing, decoding forward, pushes out the oldest frames
  of the movies that are idle; two movies that take turns (a cut between two clips) both keep hitting the
  cache as long as what they need together fits the budget (tested: 12 frames of two movies in a 12-frame
  cache, 0 decodes on repeat); when it does not fit, plain LRU thrashes on a cyclic pattern, as any LRU does.
  No movie is starved for good: a request that has to decode inserts its frame as the newest one, so the
  movie asked for again gets its recent frames back, and they stay while it is in use (tested: an idle
  movie's frames are gone after the other plays 40 frames, and are hits again after one request each).
- **Closing a movie frees its bytes.** A movie holds a lease on the cache; dropping the movie drops the lease
  and removes its frames. The worker's pool clear (a ninth open movie closes the other eight) and a rebuilt
  worker after a decoder panic do exactly that, and the new worker keeps the same cache.
- **Frame count.** At most 512 frames per movie (`DEFAULT_CACHE_FRAMES`, lowered with
  `Movie::set_cache_frames`) and 4096 in all (`DEFAULT_SHARED_CACHE_FRAMES`). These bound the bookkeeping for
  pictures under about 2 MB; from 480p up the byte budget is the bound.
- **Locking.** One `Mutex` guards the cache's books. It is held only for a lookup or an insert, never while a
  frame is decoded, converted or freed. A poisoned lock (a thread panicked while holding it) does not panic
  anyone: the next caller empties the cache, clears the poison and carries on; what it held can be decoded
  again.
- `Movie::set_cache_bytes` changes the budget of the cache the movie uses, **so for a shared cache it changes
  it for every movie**; `FrameCache::set_budget_bytes` does the same on purpose.

Measured in a real movie (one movie, so the same as before): 8 frames (992 MB) at 7424 × 4176 and 18 frames
(1,035 MB) at 5056 × 2844. The cache counts what it holds: a frame the caller still has in hand stays alive
after the cache drops it.

Beside the cache, a running decoder hands pictures back in display order, and the ones produced before they
were asked for wait in the `ready` map: at most 32 pictures and 256 MiB per movie (the lowest-numbered one
always stays, so a picture over the cap is held alone). At 8K UHD that is 2 pictures at 10 bits and 5 at
8 bits. Timeline thumbnails are not part of the shared cache: each movie has its own thumbnail cache, at most
256 pictures and 256 MiB (the app asks for thumbnails at most 256 pixels high, about half a megabyte at the
largest, so a movie's thumbnails stay near 0.12 GiB).

### Ceiling 3: the combined peak

**Decoders of the movies decoding at once + the shared 1 GiB + thumbnails (+ hold-over).** The Video track's
worker decodes one picture at a time, so one decoder is at its working-memory peak at any moment, and the
cache is 1 GiB once, not once per movie. Computed worst cases for the moment one movie is decoding, in GiB:

| | Decoder (the one decoding) | Shared cache | Hold-over | Thumbnails | Worst case |
|---|---|---|---|---|---|
| AV1 | 1.6 | 1 | 0.25 | 0.25 | 2.85, or 3.1 with thumbnails |
| HEVC | 1 | 1 | 0.25 | 0.25 | 2.25, or 2.5 |
| VC-3 | 1 | 1 | none (intra-only) | 0.25 | 2.0, or 2.25 |

So one 8K AV1 movie can reach about 1.6 + 1 + 0.25 GiB. In practice it stays under that, because the
decoder rarely needs its whole estimate. **Measured** over a whole open movie played forward (open, then
every frame in turn, no thumbnails, at the 1.5 GiB budget): 2.08 GiB for 4:2:0 at 7424 × 4176 (14 frames,
cache full at 8 frames) and 2.15 GiB for 4:4:4 12-bit with film grain at 5056 × 2844 (24 frames, cache full at
18 frames). One movie sees the same figures with the shared cache as it did with its own.

**What several open movies add.** Decision 6 shares the cache, not the decoders: each movie's decoder keeps
its own working-memory budget (decisions 5, 5a and 5b). The cache term of the sum is the same 1 GiB however
many movies are open, which is what used to grow eight-fold. A decoder term is added for each movie that
is decoding at once; the app decodes one picture at a time, so that is one. **A movie that has decoded
keeps its decoder between requests**, though (so playback can carry on from where it was, and a
sequential reader does not restart at every frame), and the worker keeps up to eight movies open
(`MAX_OPEN_MOVIES`). An idle movie therefore still holds what its decoder keeps between pictures: for
AV1 the reference slots (up to eight; 0.94 GiB for 8K UHD 4:2:0, computed as 8 × 126,858,240 bytes), for HEVC
and H.264 the decoded-picture buffer, and nothing for VC-3, ProRes and Motion JPEG, which decode one
sample at a time. This is not capped across movies by any decision; it is the figure to watch when
several large long-GOP files are open at once.

### Other limits

- `threads` (rayon) is off by default and unchanged; `Movie` runs each decoder on one thread. CI lints and
  tests it (`cargo xtask ci`).
- A request for a picture the decoder never outputs (a damaged sample) gives up once the decoder has
  output 8 pictures past it (`PAST_PICTURES_BEFORE_GIVING_UP`), not at the first. A file whose output order
  disagrees with its container timestamps by up to 7 pictures (B-frames with a missing or wrong `ctts`, or
  an H.264 stream that understates its reorder depth) still returns every frame, and a missing picture
  costs about 8 pictures of decoding, not the rest of the movie.
- AV1 `Decoder::decode` and `decode_pts` take whole temporal units: a frame whose tile groups are split
  across calls is dropped, not joined. `Movie` feeds one container sample at a time, which is a whole unit.

### Panics, `catch_unwind` and `debug_assert`

The decoders are written so that no file panics (checked arithmetic and indexing, errors as values), and
the hostile-file tests hold them to it. As a last resort the H.264, HEVC and AV1 frame workers wrap each
picture in `std::panic::catch_unwind`, so a panic that should never happen becomes a decoding error for that
picture instead of a reader blocked for ever. **That net works only where panics unwind.** Under
`panic = "abort"` (a profile setting of whoever builds the binary) and on `wasm32`, where unwinding is not
available, `catch_unwind` does nothing and a panic ends the process. This workspace sets no `panic = "abort"`,
so the native app has the net; the web build does not, and relies on the code being right. Nothing depends
on the net, and the crate uses no `debug_assert!`: an invariant a file can reach is a checked path or a
clamped value, so a debug build and a release build behave the same.

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
| 2026-10-23 | minor | Limits rewritten after the independent review: Gil's decision 5 as amended (AV1 decoder budget 1.5 GiB, HEVC and VC-3 1 GiB, frame cache 1 GiB by bytes), measured and computed ceilings per codec, which limit an 8K file hits first, the `catch_unwind` caveat (no-op under `panic = "abort"` and on wasm32), open-GOP and mixed-depth HEVC, `threads` in CI |
| 2026-10-23 | minor | Gil's V4 decisions: AV1 decoder budget 1.6 GiB (decision 5b), so full 8K UHD at 4:2:0, 8 and 10 bits, decodes, with the new admitted-size table and measured peaks (one fresh process per layout); one frame cache for the whole app, 1 GiB, counted in bytes, least recently used first across movies (decision 6), replacing the per-movie figure; the three ceilings restated (per decoder, shared cache, combined peak); the AV1 refusal at open reads the declared maximum size; a GOP request gives up after 8 pictures past the one asked for |
