# Video test fixtures

Where every sample under `crates/video/tests/fixtures/` came from, so anyone can check it, remake it or
replace it. Each sample is tiny synthetic test-pattern video (ffmpeg's `testsrc2`; no footage, no real
people, no real audio). Each has a row in [`ATTRIBUTION.md`](../../../../ATTRIBUTION.md), which carries the
licence wording; this file carries the evidence.

> **Last reviewed:** 2026-10-23 · **Last updated:** 2026-10-23 · **Change:** minor (created from the
> independent review of G-17; added the open-GOP HEVC sample)

The samples were made outside the repository, in cloud scratch, by `make-fixtures.sh` (kept with the
G-17 plan, not in this repository) and the open-GOP one by the commands below. Nothing here was made by
hand-editing bytes except the two same-length clean-ups described under "Cleaning".

## Tools

| Tool | Version | Licence |
|---|---|---|
| ffmpeg (muxing, `testsrc2`, reference decodes) | 6.1.1-3ubuntu5 (libavcodec 60.31.102, libavformat 60.16.100), Ubuntu noble | The binary is built `--enable-gpl` (GPL-2.0-or-later) |
| libx265 (HEVC encoder) | x265 3.5+1-f0c1022b6 (Ubuntu `libx265-199` 3.5-2build1) | GPL-2.0-or-later |
| libaom (AV1 encoder) | libaom v3.8.2 (Ubuntu `libaom3` 3.8.2-2ubuntu0.2) | BSD-2-Clause, plus the Alliance for Open Media Patent License 1.0 |
| ffmpeg native `dnxhd` (VC-3 encoder and decoder) | ffmpeg 6.1.1, libavcodec 60.31.102 | Source LGPL-2.1-or-later; this build is GPL |
| ffmpeg native `hevc` (HEVC reference decoder) | same ffmpeg | as ffmpeg |
| libdav1d (AV1 reference decoder) | 1.4.1 | BSD-2-Clause, plus the AOM patent licence; cross-checked against `libaom-av1` decodes |

The encoders' licences do not reach the samples or this repository: the files are encoder output and no
encoder code is included. That is a legal reading, noted in `docs/video-codecs.md`, not a ruling.

## The samples

"Reference decode" is the SHA-256 of ffmpeg's raw decode of the whole sample (every frame, in output
order, planar, `-fps_mode passthrough`), recorded when the sample was made. The tests recompute it from
this repository's own decoder; no external tool runs in CI. Sizes and SHA-256 of the file itself are
those of the committed bytes.

| File | Bytes | SHA-256 of the file | Encoder | Frames | Reference decode SHA-256 (format) |
|---|---:|---|---|---:|---|
| `hevc/hevc-main-8bit-64x64-4f.mp4` | 2064 | `410c3dc0ec700e66857c27eb0ced340c491d9d053e0f7361a82e93c662a6e98d` | libx265 3.5+1-f0c1022b6 | 4 | `fe81f18a9188107c96b53ee241b477689fdff010dcbd52cec5a1033d5ce3aa40` (yuv420p) |
| `hevc/hevc-main-8bit-64x64-4f.h265` | 1175 | `0dc048ca48b2cbc56a46ac1ac7ebcae5e430ddecfd1763ae3e329e6b99385a90` | libx265 3.5+1-f0c1022b6 | 4 | same as the MP4 (checked) |
| `hevc/hevc-main10-64x64-4f.mp4` | 2080 | `bc0c725e57e2201f0b15e4fc87f51f5f94392781b34708a7b8d67d5383c96ecb` | libx265 3.5+1-f0c1022b6 | 4 | `3fe0777f821dd7c0ef982a00660a54b80dd24eefb8da4be121c61e2f86876362` (yuv420p10le) |
| `hevc/hevc-main-8bit-64x64-bframes2-ref2-48f.mp4` | 9747 | `9dcd82d5e2a219fd9b2ad1e359d2d71ad33c763c4454057b244bb083099a9130` | libx265 3.5+1-f0c1022b6 | 48 | `9a995aa1c9902ac309b945ec3126447f2212e97bc2285f5a84b49c6e62030298` (yuv420p) |
| `hevc/hevc-main-8bit-64x64-opengop-32f.mp4` | 9031 | `ea67062b5250e68840518ebbe099d510729f4fd2915b5146e86980fa8c3dafdc` | libx265 3.5+1-f0c1022b6 | 32 | `0e6b618e54886fb02efb3ea389cef9f9e5da87308be5c657094cb1efbe6668cf` (yuv420p) |
| `av1/av1-8bit-64x64-4f.ivf` | 1116 | `5c4e6af2c8fa0cb21f2d8d0e738b1edbdd13292e73f3c101774be8d4fba9071b` | libaom v3.8.2 | 4 | `40a57f9181ba144228a4d7701268fc5d82ffedc7c570ae089ea54cb6941f28ac` (yuv420p, libdav1d = libaom) |
| `av1/av1-8bit-64x64-4f.mp4` | 1836 | `60d5ce9960dbebe3f43fc0b286fc8b111bb00b7ad27212d52ee2d66e17e7826f` | libaom v3.8.2 | 4 | same as the IVF (checked) |
| `av1/av1-10bit-64x64-4f.ivf` | 1107 | `73a017e8eb31410459c9884c64e7cd0fb27717f9fb81383db72b31eeedc775bb` | libaom v3.8.2 | 4 | `b4dc4960c00d3d5a2cf9e712da0ec1742e714952b24bbd401a862637ba9274a3` (yuv420p10le, libdav1d = libaom) |
| `av1/av1-10bit-64x64-4f.mp4` | 1827 | `df946fdd13ef2c2a5ebf022d209c722d47f1e94cbbde16ffa700602053b73278` | libaom v3.8.2 | 4 | same as the IVF (checked) |
| `av1/regress-grain-points.frames` | 2514 | `96f3305a0ed73dcaf7978fb67f8381a4fd58f19b5290997a9c601f13e6e5977e` | libaom (see below) | 1 unit | none: an error-path input |
| `av1/regress-scaled-reference.frames` | 2593 | `703915e1c1a3e1d64d969243fe91b78fe2d03f7588ade967cce7258da4f26567` | libaom (see below) | 6 units | none: an error-path input |
| `vc3/dnxhr-lb-256x144-2f.mov` | 17127 | `64d36be0c821329d66457b07824b0677a3f2af8a0357a8fe2e04bf570b68c1a0` | ffmpeg 6.1.1 native `dnxhd`, profile `dnxhr_lb` | 2 | `5d9c40138ae48f2ab5d854fd709310cb9facbcbc16b82e4c80115ceb66d7eb27` (yuv422p) |

The VC-3 test (`src/vc3/fixture_tests.rs`) compares against ffmpeg's decode sampled on a macroblock
grid, to within two levels per sample, rather than against that whole-file hash: this decoder's integer
transform differs from ffmpeg's by that much. The HEVC and AV1 tests compare against the hashes above
bit for bit.

## Commands

Shorthand used below (all of it is literal; nothing else is set):

```sh
FF="ffmpeg -hide_banner -y -loglevel error"
STRIP="-map_metadata -1 -fflags +bitexact -bitexact"
STRIP_STREAMS="-map_metadata -1 -map_metadata:s:v -1 -map_metadata:s:a -1 -fflags +bitexact -bitexact"
SRC64="-f lavfi -i testsrc2=size=64x64:rate=24"
SRC256="-f lavfi -i testsrc2=size=256x144:rate=24"
```

The strip flags go on the output side (on the input they do nothing). Encoding straight to MP4 writes an
`encoder=Lavc libx265` tag that no option removes, so each MP4 is a two-step: encode to a tagged MP4,
then a stream-copy remux with the `STRIP_STREAMS` flags, then `zero-muxer-strings.py` (see "Cleaning").

HEVC (libx265):

```sh
# hevc-main-8bit-64x64-4f.h265 (raw Annex-B) and .mp4
$FF $SRC64 -frames:v 4 $STRIP -pix_fmt yuv420p -c:v libx265 -x265-params info=0 -f hevc hevc-main-8bit-64x64-4f.h265
$FF $SRC64 -frames:v 4 $STRIP -pix_fmt yuv420p -c:v libx265 -x265-params info=0 work/hevc-main-8bit-64x64-4f.tagged.mp4
$FF -i work/hevc-main-8bit-64x64-4f.tagged.mp4 $STRIP_STREAMS -c copy hevc-main-8bit-64x64-4f.mp4
# hevc-main10-64x64-4f.mp4
$FF $SRC64 -frames:v 4 $STRIP -pix_fmt yuv420p10le -c:v libx265 -profile:v main10 -x265-params info=0 work/hevc-main10-64x64-4f.tagged.mp4
$FF -i work/hevc-main10-64x64-4f.tagged.mp4 $STRIP_STREAMS -c copy hevc-main10-64x64-4f.mp4
# hevc-main-8bit-64x64-bframes2-ref2-48f.mp4 (27 B pictures, 20 P, 1 I; reordered output)
$FF $SRC64 -frames:v 48 $STRIP -pix_fmt yuv420p -c:v libx265 -x265-params bframes=2:ref=2:info=0 work/hevc-main-8bit-64x64-bframes2-ref2-48f.tagged.mp4
$FF -i work/hevc-main-8bit-64x64-bframes2-ref2-48f.tagged.mp4 $STRIP_STREAMS -c copy hevc-main-8bit-64x64-bframes2-ref2-48f.mp4
# hevc-main-8bit-64x64-opengop-32f.mp4 (open GOP: see below)
$FF $SRC64 -frames:v 32 $STRIP -pix_fmt yuv420p -c:v libx265 -x265-params open-gop=1:keyint=8:min-keyint=8:bframes=2:info=0 work/opengop.tagged.mp4
$FF -i work/opengop.tagged.mp4 $STRIP_STREAMS -c copy hevc-main-8bit-64x64-opengop-32f.mp4
```

(Remuxing the raw Annex-B stream into MP4 with `-c copy` dropped 2 of 4 B pictures, which is why the
MP4s are made from a tagged MP4 encode and the `.h265` is a separate encode with the same options; the
decoded frames are identical.)

AV1 (libaom), IVF encode then MP4 stream copy:

```sh
$FF $SRC64 -frames:v 4 $STRIP -pix_fmt yuv420p -c:v libaom-av1 -crf 40 -b:v 0 -cpu-used 8 -f ivf av1-8bit-64x64-4f.ivf
$FF -i av1-8bit-64x64-4f.ivf $STRIP_STREAMS -c copy av1-8bit-64x64-4f.mp4
$FF $SRC64 -frames:v 4 $STRIP -pix_fmt yuv420p10le -c:v libaom-av1 -crf 40 -b:v 0 -cpu-used 8 -f ivf av1-10bit-64x64-4f.ivf
$FF -i av1-10bit-64x64-4f.ivf $STRIP_STREAMS -c copy av1-10bit-64x64-4f.mp4
```

VC-3 (DNxHR LB, 4:2:2 8-bit), raw elementary stream then MOV stream copy:

```sh
$FF $SRC256 -frames:v 2 $STRIP -pix_fmt yuv422p -c:v dnxhd -profile:v dnxhr_lb -f dnxhd work/dnxhr-lb-256x144-2f.dnxhd
$FF -framerate 24 -f dnxhd -i work/dnxhr-lb-256x144-2f.dnxhd $STRIP_STREAMS -c copy dnxhr-lb-256x144-2f.mov
```

Reference decodes (the SHA-256 in the table is `sha256sum` of each output file, or of the piped bytes):

```sh
$FF -c:v hevc    -i <sample>.mp4 -fps_mode passthrough -f rawvideo -pix_fmt yuv420p    out.yuv   # Main 8-bit, B frames, open GOP
$FF -c:v hevc    -i hevc-main10-64x64-4f.mp4 -fps_mode passthrough -f rawvideo -pix_fmt yuv420p10le out.yuv
$FF -c:v libdav1d -i av1-8bit-64x64-4f.ivf  -fps_mode passthrough -f rawvideo -pix_fmt yuv420p    out.yuv   # = libaom-av1
$FF -c:v libdav1d -i av1-10bit-64x64-4f.ivf -fps_mode passthrough -f rawvideo -pix_fmt yuv420p10le out.yuv
$FF -c:v dnxhd   -i dnxhr-lb-256x144-2f.mov -fps_mode passthrough -f rawvideo -pix_fmt yuv422p    out.yuv
```

Cross-checks made at the time: every MP4 or MOV decodes to the same frames as the raw or IVF stream it
came from; the AV1 references are identical between libdav1d and libaom-av1; frame counts equal decoded
bytes divided by the frame size.

## The open-GOP HEVC sample

`hevc/hevc-main-8bit-64x64-opengop-32f.mp4` exists so that random access into an open GOP is tested against
a real encoder, not only against hand-built streams. Made on 2026-10-23, 32 frames, 64 x 64, 24 fps,
8-bit 4:2:0, Main profile. Its structure, by NAL unit type in decode order:

- one IDR_N_LP picture (type 20), then three CRA pictures (type 21), each at a sync sample of the MP4;
- three RASL leading pictures (types 8 and 9) after two of the CRA pictures: they come before their CRA in
  display order and predict from the GOP before it, so showing one needs a decode that starts at the
  previous random access point.

`the_open_gop_sample_has_cra_access_points_and_leading_pictures` in `tests/hevc_fixtures.rs` asserts that
structure, so the sample cannot be swapped for a closed-GOP stream unnoticed. The x265 log of the encode
reports 4 I, 10 P and 18 B pictures. The encode was run twice and gave the same file both times
(SHA-256 above).

Clean of tool names, proved by `strings` on the committed file:

```
$ strings -n 6 hevc-main-8bit-64x64-opengop-32f.mp4 | grep -i -E 'x265|x264|libaom|svt|rav1e|ffmpeg|lavf|lavc|gpl|copyright'
$ echo $?
1
$ strings -n 4 hevc-main-8bit-64x64-opengop-32f.mp4 | grep -i -E 'x265|x264|libaom|svt|rav1e|ffmpeg|lavf|lavc|gpl|copyright|handler|FFMP|encoder|options|version'
$ echo $?
1
```

(`info=0` keeps x265's settings SEI out of the stream.) What the file still holds beyond sample data is
structure: the `ftyp` brands (`isom`, `iso2`, `mp41`), `language=und` in the media header, an empty `udta`/`meta`
box whose handler is the standard four-character codes `mdir`/`appl` with no strings, and no tool, encoder
or vendor text. The other MP4s made the same way contain the same empty box.

## Cleaning

No ffmpeg option removes the muxer's default strings, so `zero-muxer-strings.py` overwrites them in
place, same length, with zero bytes (no box size or offset changes): the `hdlr` names `VideoHandler`,
`SoundHandler` and `DataHandler` (for a QuickTime Pascal-string name, the length byte too), and the MOV
sample-entry vendor field `FFMP`. Each cleaned file was decoded again and compared with its reference.
The open-GOP sample reported "cleared 1 handler name(s), 0 vendor field(s)".

## The `regress-*.frames` files

These two are not made by a documented command, and this file says so rather than guess. They came out of the
AV1 lane's mutation hunt (`PATOOLS_FUZZ_SAVE` in `tests/av1_corpus.rs`), which mutates libaom-encoded
128 x 96 `testsrc2` streams (the example command in that file's header is
`ffmpeg -f lavfi -i testsrc2=size=128x96 -frames:v 12 -c:v libaom-av1 -crf 32 -b:v 0 -f ivf a.ivf`) and keeps
any input that made the decoder panic or misbehave. Each file is `[u32 little-endian length][bytes]` per
temporal unit, as `tests/av1_regress.rs` reads them.

- `regress-grain-points.frames`: one 2510-byte unit of a 128 x 96 Main profile stream; a few header bits
  were changed so the film-grain scaling points no longer increase (a zero-width segment once divided by
  zero). Expected: refused with an "Invalid" error naming the scaling points, no picture, no panic
  (`film_grain_scaling_points_that_do_not_increase_are_refused`).
- `regress-scaled-reference.frames`: six units of a 128 x 96 stream whose sequence header reads as Professional
  profile; a damaged header gave an inter frame a size more than twice or less than 1/16 of its
  reference's, and the prediction read past its scratch buffer. Expected: an "Invalid" error saying the
  frame cannot predict from that reference, no panic
  (`a_reference_of_a_size_the_frame_cannot_predict_from_is_refused`).

The encoder was libaom in the AV1 lane's build environment; its exact version was not recorded when the
inputs were saved, and the mutations were random. Re-running the example command above with libaom v3.8.2
gives a first unit of 1035 bytes, where `regress-scaled-reference.frames` has 1041, so these bytes cannot be
regenerated from the command. Because they are error-path inputs there is no picture and so no reference
decode; the expected result is the error above. Their SHA-256 are in the table.

## Adding a sample

Make it outside the repository from a synthetic source, strip metadata as above, prove it clean with
`strings`, record the reference decode, then add: the file here and a row in the table, a row in
`ATTRIBUTION.md` (`cargo xtask assets` fails without one), and a test that uses it.
