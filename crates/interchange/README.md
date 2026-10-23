# soundcraft-interchange

AAF and OMF 2.0 import and export for Pa Tools sessions: the hand-off to and from picture editors
and post-production. Layer L2 (above `model`, `time`, `audio-io`, `brand` and `cfb`): no file I/O,
no threads, no `unsafe`. Media references are strings and embedded audio is returned as WAV bytes;
the engine reads, writes and relinks.

Ported from FilmCraft's `crates/interchange` (`aaf/`, `omf/`) at `storytold/filmcraft@f47a1f6`,
MIT OR Apache-2.0 (see `ATTRIBUTION.md` and `UPSTREAM.md`). The donor's project model and its other
formats (EDL, FCP7 XML, FCPXML, OTIO, ALE) were left behind; its container code is
[`soundcraft-cfb`](../cfb/README.md). How the engine is to use this crate is in
[`docs/g03-interchange-seam.md`](../../docs/g03-interchange-seam.md).

```rust
use soundcraft_interchange::{export, import, merge_into, ExportOptions, Format, ImportOptions};

let (bytes, report) = export(&session, Format::Aaf, &ExportOptions::default())?;
let opts = ImportOptions { sample_rate: Some(session.sample_rate.hz()), ..Default::default() };
let (fragment, report) = import(&bytes, Format::Aaf, &opts)?;
let merged = merge_into(&mut other_session, fragment, 0)?;   // then write merged.extracted, load merged.sources
```

## Specifications

Clean-room: written from public specifications only. The AAF SDK, pyaaf2, the OMF Toolkit and
other implementations were not consulted. Where a specification leaves a layout open (how a
nested sequence is written, how several video tracks share a slot), what a picture editor writes
was read off files it exports, never from the application itself.

| Format | Document | Edition | Used for |
|---|---|---|---|
| AAF | AMWA *AAF Object Specification* | v1.1 (2005) | classes, properties (ids shared with SMPTE ST 377-1 local tags), mob / slot / segment model, sequences and transitions, operation groups, parameters, definitions, descriptors, locators, essence data |
| AAF | AMWA *AAF Low-Level Container Specification* | v1.0.1 | objects as structured storages, `properties` streams, stored forms, strong / weak reference collections and their index streams, `referenced properties` |
| AAF | AMWA *AAF Edit Protocol* | v1.0 (AMWA AS-01) | top-level composition, mob chains (composition → master → file → physical source), operation definitions (dissolve, fade to black, audio gain / dissolve), usage codes |
| AAF | SMPTE RP 224 / RP 210 registers | — | data definition, class and type labels |
| AAF | Microsoft [MS-CFB] | rev. 10.0 | the container, see [`soundcraft-cfb`](../cfb/README.md) |
| OMF | *OMF Interchange Specification* | Version 2.0 (1997) | classes (`HEAD`, `CMOB`, `MMOB`, `SMOB`, `MSLT`, `TRKD`, `SEQU`, `SCLP`, `FILL`, `TRAN`, `EFFE`, `ESLT`, `CVAL`, `VVAL`, `TCCP`, `WAVD`, `AIFD`, `WAVE`, `AIFC`, locators), property and type names |
| OMF | Apple *Bento Specification* | revision 1.0d5 | container label, TOC encoding, objects / properties / types, references |
| WAVE | Microsoft RIFF WAVE | — | embedded and extracted audio files |

## Layers

| Module | Role |
|---|---|
| `lib.rs` | `export`, `import`, `merge_into`, `detect`, `Report`, options. The whole public surface |
| `session` | `Session` ⇄ composition model: tracks, clips, gain, fades, crossfades, markers, timecode, media, with caps on what an import builds |
| `comp` | format-neutral mob-style model (compositions of slots whose sequences of fillers, source clips and overlapping transitions reference media); times in `ticks` (254 016 000 000 per second) |
| `aaf` | `store` (objects on structured storage), `read`, `write`, `ids` (class, property and definition ids) |
| `omf` | `bento` (the container), `read`, `write` |
| `bytes`, `common`, `ticks`, `tags`, `wav` | checked slice and integer helpers (the only place a byte offset from a file is turned into a read), URL and path conversion, exact time, branded tag names, WAV parse and write |

Audio edits are sample-exact: sound slots use the sample rate as the edit rate, and every
conversion between frames, samples and ticks is exact or overflow-checked.

## Files are untrusted

- No indexing or slicing with a number from a file: `clippy::indexing_slicing` is denied outside
  tests and the helpers in `bytes` return `Option`.
- Every count and size is checked against the file's own length before anything is allocated;
  object graphs are walked once (a mob or slot reached twice is reused, a composition that nests
  itself is a gap), from a work budget sized by the object count; embedded audio is bounded by
  `8 × file length + 16 MiB`; an import builds at most 4096 tracks, 1 000 000 clips, 100 000
  sources and 100 000 markers.
- Container damage is an error; damage inside a readable graph is a warning and a gap.

## Not represented

MIDI, instrument, aux, master, VCA and folder tracks, mixer and automation, plug-ins, playlists
other than the active one, time stretch, muted clips (written as gaps), nested sequences (read,
left as gaps), effects other than gain and dissolves; OMF carries no markers and no picture. Each
is a `Report` entry, never an error.

## Tests

- `src/aaf/tests.rs`: written files checked against the required properties of every class
  written; embedded and video files; a store round trip of every stored form; a picture editor's
  layered-video layout; compositions that use themselves, each other, or chain forty deep.
- `src/aaf/hostile_tests.rs`, `src/omf/tests.rs`: one test per hostile structure (collections
  larger than their index, a storage or stream named twice, nesting past the limit, damaged
  property streams; hostile Bento graphs and work-budget exhaustion).
- `src/hostile_tests.rs`: whole files (AAF v3, v4, embedded; OMF, embedded) cut at every byte,
  every bit of the container header flipped, FAT / DIFAT / mini FAT chains made to loop, sizes in
  the header, directory and table of contents set absurdly high, single-byte damage with a
  per-import time bound.
- `tests/roundtrip.rs`: a demo-like session through AAF and OMF (linked, embedded, mono breakout,
  video, merge into another session).
- `tests/fuzz.rs` (proptest): damaged valid files, arbitrary bytes, and random sessions through
  both formats and back.

No FilmCraft-written AAF or OMF files are checked in: the donor ships none. Every fixture is
generated by the tests from synthetic sessions, so there is no bundled asset to attribute.
