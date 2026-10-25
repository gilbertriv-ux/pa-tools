# Pa Tools — Build Manuscript (A–Z)

A running, end-to-end record of the work that produced the alpha build
`claude/alpha-integration` @ `5de9d7f`. Kept so any collaborator (or a future
session) can reconstruct exactly how this build came together, what was decided,
and why.

- **Repository:** `gilbertriv-ux/pa-tools` (clean-room pure-Rust fork of SoundCraft)
- **Build:** `claude/alpha-integration` — code at `95ea695`, integration merge `5de9d7f`
- **Base:** `25bd43d` (the "Big Build")
- **Last updated:** 2026-10-25

---

## A — Architecture & ground rules

- Clean-room, pure-Rust DAW. No Avid/Pro Tools code or assets; no GPL/AGPL/LGPL
  code (FilmCraft, MIT/Apache, is the one allowed donor). Third-party product
  names never appear in user-visible strings.
- **Never crash:** no `unwrap`/`expect`/`panic!` in non-test code; `unsafe` only
  in the three plugin-host crates; errors are values.
- **Everything is a command:** user actions go through `CommandSpec`/`cmd!`,
  journaled and undoable.
- Crate layering enforced by `cargo xtask layers`.
- **Audio-thread realtime safety:** lock-free `rtrb` SPSC ring only; no
  Mutex/RwLock/mpsc/alloc on the callback; under-run → silence.
- **Hard gate:** never overwrite/delete/move/rename anything on Gil's drives;
  container scratch (`/tmp`, build output) may be cleaned, with the path named.

## B — Big Build (`25bd43d`), the base

The integration point for this alpha. Combines the RAM/streaming fix, MIDI thru,
and the merged G-18 speech track. (A parallel duplicate local merge, `017e6a6`,
was found to be functionally identical to Codex's pushed `25bd43d` — one blank
line plus Cargo.lock ordering — so local was reset to `25bd43d` rather than
force-pushed over shared work.)

## C — Coordination model (cloud ↔ Mac)

- This cloud session authors non-audio features, research, and integration.
- A parallel "Codex" agent on the Mac owns the audio engine (`crates/mix`,
  `crates/playback`); this session does not touch those crates.
- Agents build and test but never commit; the coordinator independently verifies
  and commits. Disk is ~30 GB — builds are serialised (never two concurrent
  cargo builds); parallel work is non-compiling authoring + research only.

## D — Streamed playback (lower RAM)

`SourceAudio` gained `Resident | Streamed` backings with a `StreamReader`, a
pre-read worker, and background peak computation. Fixes made during wiring:

- `autosave.rs` production bug writing silent files for streamed sources →
  added `load_resident()`.
- FIFO hang from a missing `is_file()` guard in `stream_pcm_source`.
- Test accessors migrated from `buffer.channels[0][0]` to `read()`.
- Mix seek test corrected; clippy `map_or` → `is_none_or` in `peaks.rs`.

## E — Speech engine (`pa-speech`)

Ported from FilmCraft (MIT/Apache). Provides background transcription with
word-level, sample-accurate timings stored on the session via
`track.set_transcript`. Only one recogniser holds the model in memory at a time.
The speech-model download lives behind the cargo `download` feature, forwarded
`apps/soundcraft → ui-egui → engine → pa-speech`, **off by default**.

## F — Feature #1: in-app model downloader (`claude/g18-model-download`, `b7b3152`)

When transcription needs its model and none is installed, the Transcript window
shows a `#[cfg(feature="download")]` **Download** button: background thread,
progress + stage reporting, `AtomicBool` cancel, target directory resolved via
`ModelSearch::from_environment(...)`. Added the optional `pa-speech` dependency
and `download` feature to `ui-egui/Cargo.toml`, 5 tests in
`ui-egui/tests/model_download.rs`, plus documentation fixes
("Text ▸ Transcript" → "Window ▸ Transcript").

## G — Feature #2: transcript word correction (`claude/g18-transcript-edit`, `13487ec`)

- **Model:** `Transcript::set_word_text(index, text)` — trims, caps at 128
  chars, refuses empty, returns whether it changed.
- **Engine:** new `track.edit_word` command with bounds + no-op/empty guards,
  mirroring `rename_speaker`; 8 new tests.
- **UI:** `Action::EditWord`, inline `word_editor()` (select-all, Esc cancels,
  empty/unchanged = no-op). Double-click opens the editor; single-click seeks;
  Shift-click extends selection. 5 new UI tests.

## H — G-03 AAF parser (`claude/g03-aaf-parser`, `1075fd3`), research + skeleton

- **Scope decision:** AAF = Microsoft Compound File (CFB) container; object model
  (Mobs/MobSlots/Segments via a GUID metadata dictionary). The AAF SDK's Public
  Source License bundles Microsoft IP → a clean-room landmine, so **spec only**,
  no SDK code.
- The CFB container + identification layers (`cfb.rs`, `aaf.rs`) already existed
  from M1 (hostile-input hardened, 12 CFB tests). Rather than duplicate them, a
  **neutral composition model** was added (`composition.rs`): `TrackKind`,
  `CompClip`, `CompTrack`, `Composition`, and `Cfb::composition()` which returns
  an explicit "unsupported" error until a real `.aaf` sample pins the object-model
  GUIDs. 3 unit tests. Kept **off** the alpha branch by instruction.

## I — Integration (`claude/alpha-integration`, `5de9d7f`)

1. Pushed the AAF skeleton branch to origin (kept separate).
2. Created `claude/alpha-integration` off `25bd43d`.
3. Merged `g18-model-download` (clean) then `g18-transcript-edit`.
4. **One conflict** in `transcript_window.rs` (the `Action` enum: rustfmt's
   multi-line layout from #1 vs the other side's single-line variants + new
   `EditWord`). Resolved by keeping the multi-line layout and adding `EditWord`;
   everything else auto-merged.
5. Pushed. Both features confirmed present.

## J — Journaling of verification

Verified on the **merged** tree (not just the source branches):
`cargo xtask layers` ok; `cargo clippy --all-targets --features download` clean;
tests green (model 315, engine 47, ui-egui lib 173, model_download 5);
`cargo fmt` clean. Heavy unrelated ui-egui integration-test binaries
(`bounce`, `parity_missing`, `edit_modes`) were skipped only because they
overflow the container's scratch-disk allowance; they are untouched by this
merge and fully compile under clippy `--all-targets`.

## K — Known gaps carried into the alpha

- **AAF** object-model decoding not implemented (needs a real `.aaf` sample).
- **Drag-and-drop** supports loading a sampler but not timeline clip-import.
- Transcribing non-speech audio returns "No speech found" (expected).

## M — Mac-side follow-ups on the branch (post-merge)

After the integration merge, the Mac/packaging side pushed two commits onto
`claude/alpha-integration`:

- `1eaac6f` — packaging(macos): bundle licence files inside the `.app`.
- `95ea695` — fix(ui,packaging): enable the **Window ▸ Transcript** menu item
  (added its path→command mapping to `MENU_WINDOWS`; the toggle path was already
  wired, so the item was merely greyed), and place `WHISPER-LICENSE.txt` beside
  the model weights in `Contents/Resources/models/whisper-base/`.

This resolved the Window ▸ Transcript gap noted in section K. (A local docs push
collided with these; resolved by rebasing the docs commit onto the new tip —
see section L.)

## L — Lessons / recurring issues solved

- **Disk discipline:** the shared `CARGO_TARGET_DIR=/tmp/target-verify` grew to
  26 GB and exhausted the session's disk allowance, surfacing as spurious `cc`
  link failures (`No space left on device`). Fix: delete the scratch target dir
  to reclaim the allowance, then rebuild; run tests crate-by-crate rather than
  the whole ui-egui test set at once.
- **Stop-hook false positive ("N unpushed commits, no remote branch"):** the
  shallow clone's fetch refspec was narrowed to `+refs/heads/pa-tools:...`, so
  git never created remote-tracking refs for the `claude/*` branches and the
  hook miscounted. Fix: widen to `+refs/heads/*:refs/remotes/origin/*`, fetch,
  and set upstream — `@{u}` then resolves with `0 0` ahead/behind. `git ls-remote`
  is the authoritative check.
- **Avoid duplicate work:** pushed back on a "CFB skeleton" ask that would have
  duplicated existing M1 code, and on a screech-fix task based on a retracted
  premise (flagged a SHA mismatch + that a full-branch merge = 15 conflicts vs a
  one-file cherry-pick).

## Z — Current state

| Branch | Commit | State |
|---|---|---|
| `claude/alpha-integration` | `95ea695` (+docs) | Pushed. Code tip + macOS packaging. **The DMG build target.** |
| `claude/g03-aaf-parser` | `1075fd3` | Pushed, standalone. Awaits a real `.aaf` sample. |
| `claude/g18-model-download` | `b7b3152` | Pushed, merged into alpha. |
| `claude/g18-transcript-edit` | `13487ec` | Pushed, merged into alpha. |

Next candidates: add timeline clip-import drag-and-drop; obtain a sample `.aaf`
to advance the parser.
