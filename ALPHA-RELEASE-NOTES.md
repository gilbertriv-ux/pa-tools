# Pa Tools — Alpha Release Notes

**Build:** `claude/alpha-integration` (code at `95ea695`; docs on top)
**Base:** `25bd43d` (the "Big Build"); integration merge `5de9d7f`
**Date:** 2026-10-25
**Target:** macOS Desktop DMG

This alpha folds together the streamed-playback engine work and the complete
speech-to-text feature set on top of the Big Build. It is an internal alpha for
evaluation, not a production release.

---

## New in this build

### Streamed playback (lower RAM)
Audio sources can now play from disk instead of being held entirely in memory.
Large sessions that previously loaded every file into RAM stream their audio
through a lock-free ring on a background reader, so memory use no longer scales
with total project length. Realtime safety is unchanged: the audio thread never
allocates, locks, or blocks, and an under-run falls back to silence rather than
a glitch or crash.

### Speech transcription
A track's audio can be transcribed to timed, word-level text in the
**Transcript** window. Transcription runs as a background job and stores its
result on the session; word bounds are sample-accurate from session start (the
same origin as markers and clips). Speaker labels are supported and can be
renamed.

### In-app model downloader
The first time transcription needs its speech model and none is installed, the
Transcript window offers an in-app **Download** button. The download runs on a
background thread, reports progress, and can be cancelled; the model is written
to the standard per-user config location where the next transcription looks for
it. (Build with the `download` feature enabled.)

### Transcript text correction
Any transcribed word can be corrected in place: double-click the word to open an
inline editor, edit the text, and commit. The change is applied through the
normal command system, so it is journaled and undoable. Single-click still seeks
to the word; Shift-click extends a selection.

---

## Fixed since the integration merge (`5de9d7f`)

- **Window ▸ Transcript menu item now opens the window** (`95ea695`). The toggle
  path was already wired; the item was greyed only because its path→command
  mapping was missing from `MENU_WINDOWS` in `menus.rs`. Adding it enables the
  item. (Transcribe Track already opened the window, so this is parity polish.)
- **macOS packaging bundles licence files inside the `.app`** (`1eaac6f`,
  `95ea695`), with `WHISPER-LICENSE.txt` placed beside the model weights in
  `Contents/Resources/models/whisper-base/`.

## Known limitations

- **AAF import is not functional yet.** The interchange crate reads and
  identifies the AAF container (Compound File) and defines the neutral
  composition model an import maps into, but the AAF object-model decoding
  (Mobs / MobSlots / Segments resolved through the metadata dictionary's
  class/property GUIDs) is **not implemented**. It returns an explicit
  "unsupported" error rather than guessing a layout. Completing it requires a
  real `.aaf` sample to pin the GUIDs and byte layouts. The AAF work lives on a
  separate branch (`claude/g03-aaf-parser`) and is **not** part of this build.

- **Drag-and-drop is limited to the sampler.** Dropping an audio file loads it
  into an instrument track's sampler (Sampler window / mix INSTRUMENT row).
  Dropping audio onto the arrange/timeline to create a clip is **not** supported
  yet. (See "Drag-and-drop" below.)

- **Transcription of non-speech material returns "No speech found."** Music or
  other non-voice audio will not produce a transcript; this is expected
  recogniser behaviour, not a failure.

---

## Drag-and-drop status

Present, but partial:

| Surface | Supported? |
|---|---|
| Drop audio onto an instrument track / Sampler window → loads the sampler | ✅ Yes |
| Internal drag-reorder of tracks and name plates | ✅ Yes (widget drag) |
| Drop audio onto the arrange/timeline → create a clip | ❌ Not yet |

To add timeline clip-import drag-and-drop we would:
1. Read `ctx.input(|i| i.raw.hovered_files / dropped_files)` in `edit_window.rs`
   (the same egui mechanism the sampler already uses in `sampler_window.rs`).
2. Map the drop pointer position to a track lane + timeline position.
3. Load the dropped file's bytes through the shell's file reader and create the
   source + clip via the existing command path (journaled/undoable), reusing the
   sampler's `DroppedFileHandle` byte-reading flow.
4. Draw a drop-target highlight on the hovered lane, mirroring the sampler's
   "Drop an audio file here" affordance.

This is additive and stays clear of the audio engine (`crates/mix`,
`crates/playback`).

---

## Verification (merged tree)

- `cargo xtask layers` — ok
- `cargo clippy --all-targets --features download` — clean
- Tests green: model 315, engine 47, ui-egui lib 173, model_download 5
- `cargo fmt` — clean
- No changes to `crates/mix` or `crates/playback`
