# G-03 seam: AAF and OMF in the engine

> **Last reviewed:** 2026-10-23 · **Last updated:** 2026-10-23 · **Change:** major (created) · **Owner:** the lane that holds the `crates/engine` lock

The AAF and OMF code is done as two engine-free crates. This file is the contract for the thin
engine wiring that makes it reachable, deferred because it edits `crates/engine`. Nothing in the
engine, mix or egui crates was touched by the port.

## What exists

| Crate | Layer | Package | What it is |
|---|---|---|---|
| `crates/cfb` | L0 | `soundcraft-cfb` | Compound File Binary reader and writer (v3 512-byte and v4 4096-byte sectors). Borrowed bytes in, no I/O. |
| `crates/interchange` | L2 | `soundcraft-interchange` | AAF (Edit Protocol over structured storage) and OMF 2.0 (Bento) import and export. Depends on `model`, `time`, `audio-io` (WAV only), `brand`, `cfb`. No file I/O, no threads, no `unsafe`. |

Both were ported from FilmCraft (`storytold/filmcraft` at `f47a1f6`, MIT OR Apache-2.0; rows in
`ATTRIBUTION.md`). The donor's model (a project) was replaced by `soundcraft-model::Session`;
everything below the mapping was hardened for hostile input (see "Files are untrusted").

### The whole API the engine needs

```rust
use soundcraft_interchange::{
    detect, export, import, merge_into, Format, ExportOptions, ImportOptions, MediaMode,
    Imported, Merged, ExtractedMedia, Report, Error,
};

detect(bytes: &[u8], extension: Option<&str>) -> Option<Format>        // Aaf | Omf, from content, then extension
export(&Session, Format, &ExportOptions) -> Result<(Vec<u8>, Report), Error>
import(&[u8], Format, &ImportOptions)    -> Result<(Imported, Report), Error>
merge_into(&mut Session, Imported, at: Samples) -> Result<Merged, Error>
```

- `ExportOptions { name, media: Link|Embed, handles_ms (1000), breakout_to_mono, bits: 16|24|32 (24),
  small_sectors (AAF v3), include_video (AAF only) }`; all serde, `Default` is a linked, 4096-byte
  sector, 24-bit file.
- `ImportOptions { base_dir, sample_rate, name, composition }`; pass `sample_rate: Some(session
  rate)` always, because `merge_into` refuses a fragment at another rate.
- `Report` is a list of `{level: Info|Warning, message, count}` (capped at 200 distinct entries);
  it is `Serialize`. Unsupported features never error: they are report entries.
- `Imported { session, extracted }` is a standalone `Session` fragment (ids local to it) plus
  embedded audio as complete WAV files. `Merged { tracks, sources, extracted }` is what
  `merge_into` added, with ids and paths as in the target.

## What maps

| Session | File |
|---|---|
| sample rate, frame rate, session start timecode | composition edit rate, timecode slot |
| audio track (active playlist) | sound slot; one per channel with `breakout_to_mono` |
| video track (AAF only) | picture slot |
| clip: source, offset, start, length | source clip on a master mob; sample-exact |
| clip gain, gain envelope | `MonoAudioGain` with constant or varying amplitude (dB segments split so the file's linear interpolation follows the session's) |
| fade in / out, crossfade (overlapping clips) | transition; shape (all five) kept in a tagged value |
| memory locations | comment markers (AAF only) |

Reported, not mapped: MIDI, instrument, aux, master, VCA and folder tracks, mixer, automation,
plug-ins, non-active playlists, time stretch, muted clips (written as gaps), OMF markers and
picture, nested compositions (read, then left as gaps with a warning), effects other than gain and
dissolves.

## Engine commands to add

Follow the existing `cmd!` pattern in `crates/engine/src/cmd/file.rs`. Both leaves already exist in
the incumbent menu tree (`crates/engine/catalog/menus.txt`), so the catalog needs no new line.

### 1. `file.import_session_data` (extend, undoable)

`File > Import > Session Data...` is implemented for `.scraft` only
(`crates/engine/src/cmd/setup_more.rs`). Keep its id and parameters and dispatch on content:

```
bytes = io::read_file(path)      // honours the automation read root
match soundcraft_interchange::detect(&bytes, extension_of(path)) {
    Some(format) => import_interchange(e, p, &bytes, format),   // new
    None         => existing .scraft path (read_text, Session::from_json)
}
```

`import_interchange`, in this order:

1. `ImportOptions { base_dir: parent(path), sample_rate: Some(e.session().sample_rate.hz()),
   composition: p.composition (default 0), name: None }`. On `Err`, return the error text as the
   command error (`EngineError::Io(format!("{path}: {err}"))`); never unwrap.
2. Optional `tracks: [names]` (as in the `.scraft` path): keep only those tracks of
   `imported.session`, then drop every `Source`/`VideoSource` no remaining clip uses and every
   `ExtractedMedia` for a dropped source. An unknown name is a `bad(id, ...)` error, as today.
3. `at` (default 0, `position_param`): `merge_into(e.session_mut(), imported, at)` in the command
   body, so the engine's one-step undo covers the tracks, sources, videos and markers.
4. For each `Merged.extracted`: write `wav` below the session folder at `path` with
   `io::make_dir` and `io::write_path` (both honour the automation write root). The crate names
   the file `Audio Files/<name>[-n].wav` from the file's media name, sanitised to letters,
   digits, space and `-_.`, so it cannot leave that folder; but the number is unique only inside
   the fragment. **Never overwrite**: pick the name with `io::unique_audio_rel` and also skip any
   name already on disk, write that, and set the merged `Source.path` to it. A session with no
   folder yet and any extracted media is an error that says to save the session first.
5. Load audio for every `Merged.sources` audio source exactly as `import_session_data` does for a
   `.scraft` source: `io::stored_path`, then `io::decode_file`, insert in `session.pool`, set
   `frames` from the decoded buffer and `unsaved = true`; a source that fails to load stays in
   the session with `unsaved = false` and its path goes in `missing` (offline, relinkable).
   Linked paths come from the file and are untrusted: `stored_path` gates them and the engine
   only ever reads them.
6. Video sources: `io::relink_videos(session, folder)` as `file.import_video` does.
7. If the target session has no clips and `adopt_timing: true` is passed, copy `frame_rate` and
   `timecode_start` from the fragment (taken from `imported.session` before the merge). `merge_into`
   deliberately does not change the target's timing.
8. Select the new tracks (`edit.selected_tracks`), as the `.scraft` path does.

Result: `{"tracks": [..], "sources": n, "missing": [..], "format": "AAF"|"OMF",
"report": [{"level": "warning", "message": "...", "count": 1}, ..]}`. Expose the report in the
result so agents and the UI show what the file could not carry.

### 2. `file.export_selected_tracks_as_aaf_omf` (new, `noundo`, `has_selection`)

Menu leaf `File > Export > Selected Tracks as New AAF/OMF...`, category `["File", "Export"]`.
Model it on `file.export_selected_tracks_as_session`:

```
{path, tracks?, format?: "aaf"|"omf", media?: "link"|"embed", handles_ms?, breakout_to_mono?,
 bits?: 16|24|32, small_sectors?, include_video?, name?, replace?: false}
```

1. `refuse_replacing(e, id, &path, p)`: the repository rule is that nothing is overwritten.
2. `format`: from the parameter, else `Format::from_extension` of `path`; neither is a
   `bad(id, "`format` or a .aaf/.omf path required")`.
3. `keep = tracks_required(...)`; clone the session; retain the kept tracks (plus the master, as
   the session export does; the crate reports and skips non-audio, non-video tracks).
4. Set `clone.folder` to the session folder (`e.path` parent). **Required**: relative
   `Source.path` values become absolute locators only through `Session::folder`; without it the
   report says the paths were written relative.
5. `media: embed` needs the audio in `session.pool`; the crate links any source that is not
   loaded and says so in the report. Embedded size is refused above `MAX_EMBED_BYTES` (2 GiB).
   Run `io::refuse_longer`-style checks first so a long session gives a clear error, not a stall.
6. `let (bytes, report) = soundcraft_interchange::export(&clone, format, &opts)?`, then
   `io::write_file(&path, &bytes)` (which honours the automation write root; on wasm the same call).

Result: `{"path": path, "format": "AAF", "bytes": n, "report": [..]}`.

### 3. Agents and tests

- Every command is a control-protocol command and an MCP tool already (`docs/mcp.md`,
  `docs/control-protocol.md`); the `cmd!` help string is the tool description. Nothing else to
  register unless that registry is still hand-listed on the merge target.
- Engine tests the wiring must add (the crate's own tests do not cover the engine):
  demo engine → export AAF and OMF to a temp dir → import into an empty engine → clip positions,
  lengths, offsets, gains and fades equal; embed round trip writes `Audio Files/*.wav` and
  loads them; importing twice never overwrites the first import's files; a truncated file, a
  random file and a `.scraft` mis-named `.aaf` return `Err` and leave the session unchanged;
  undo after import removes tracks, sources, videos and markers in one step; the export refuses an
  existing path unless `replace`.
- Parity: the export leaf flips from unimplemented to implemented, so regenerate with
  `cargo xtask parity` and record the delta that actually appears. `Import > Session Data` was
  already counted.

## Files are untrusted

The crates are written for input from anyone: an importer error is a value, never a panic or a
hang, and nothing is sized by a number the file states before it is checked against the file's
own length.

- Limits derive from the input's size and have absolute ceilings; the container reader bounds
  directory entries and total bytes read (`4 × length + 1 MiB`), claims every sector once so
  chains that loop, overlap or leave the file are errors, and detects FAT, DIFAT and mini FAT
  cycles.
- The importer builds at most 4096 tracks, 1 000 000 clips, 100 000 sources, 100 000 markers, and
  reads embedded audio up to `8 × file length + 16 MiB`.
- Compound-file damage (cut file, bad header field, looping chain, impossible size) is an `Err`.
  Damage inside the object graph that leaves a readable file (a clip that points nowhere, a
  composition that contains itself) imports with a warning and a gap.
- `clippy::indexing_slicing`, `unwrap_used`, `expect_used` and `panic` are denied in the crates'
  non-test code.

So the engine can call `import` on a file straight from the user. It still must not read a file
larger than it would read for any other import (`io::read_file` already reads whole), and it must
treat paths inside the file as untrusted (read-only, never written to).

## Known limits to tell the user about

- OMF carries no markers and no picture; AAF carries both.
- Fade shapes round trip through this application's own files; other applications keep the
  fade length and a standard dissolve.
- Nested sequences from picture editors are read and left as gaps with a warning.
- No file from real editing software or a real Pro Tools session is in the repository, so
  compatibility with them is by specification, not by test. Collect such files, make small ones
  fixtures (with an `ATTRIBUTION.md` row each), and bring the failures to this crate.
- `.ptx` is out of scope (proprietary, undocumented).
