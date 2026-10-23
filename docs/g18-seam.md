> **CURRENT STATE — read first (updated after the skeleton + wiring landed).**
> `crates/pa-speech` ALREADY EXISTS on this branch as a compiling, tested skeleton: the seam types
> below, `FixedTranscriber`, `downmix`, `word_error_rate`, the pinned model `catalogue` in
> `models.rs`, a gated `load()`, the no-model test, README and `.gitignore`. It is clippy-clean and
> `cargo xtask layers` passes. The engine/model wiring (the `track.transcribe` command + the
> `Transcript` store in `crates/model`) is also built and green. **Codex's remaining job is the
> feature-gated FLESH-OUT, not creating the crate:** port the donor's `whisper/` module + `mel.rs` +
> `vad.rs` + `diarize.rs` behind the `whisper` feature (converting the donor's `Tick` times to this
> crate's `f64` seconds), the HTTPS downloader behind the `download` feature, and the gated
> `whisper_model` test. Do NOT recreate the seam types or change their public shapes — the engine
> depends on them. Everything below is the fixed contract.

# G-18 seam — `crates/pa-speech` (authoritative contract for the Codex port)

This file is the exact contract the Codex assistant builds to. It was derived by reading the
donor `storytold/filmcraft` `crates/speech` (package `filmcraft-speech`) directly, not from memory.
Claude wires the engine/model/UI around this seam; Codex builds the crate to match it.

## Crate identity
- Directory `crates/pa-speech`, package **`soundcraft-pa-speech`**, short name `pa-speech`.
- **Standalone, layer 0.** No `soundcraft-*` or `filmcraft-*` dependency. The donor's only workspace
  deps are **`filmcraft-time`** (`Tick`, `TICKS_PER_SECOND`) and **`filmcraft-project`** (`Transcript`,
  `Word`, `Speaker`) — no `filmcraft-audio-io`, so `downmix(&[Vec<f32>]) -> Vec<f32>` and
  `load(&Path, &str)` are already `f32`/path-pure. **Inline both crates throughout the port** — not just
  `lib.rs`, but also `whisper/mod.rs`, `vad.rs` and `diarize.rs`, which all `use filmcraft_project::…` /
  `filmcraft_time::…`. If any ported code reaches for another workspace type, inline it the same way
  (times as `f64` seconds); add no `filmcraft-*`/`soundcraft-*` dependency.
- `#![forbid(unsafe_code)]` (candle is pure Rust).

## Public API (port faithfully; inline the time/transcript types)
```rust
pub const SAMPLE_RATE: u32 = 16_000; // every transcriber takes MONO 16 kHz; caller resamples first

#[derive(Clone, Debug, PartialEq)]
pub struct Word { pub start_sec: f64, pub end_sec: f64, pub text: String, pub speaker: u8 }

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Transcript { pub words: Vec<Word>, pub language: String, pub source: String }

#[derive(Clone, Debug, PartialEq)]
pub struct Options { pub language: Option<String>, pub diarize: bool, pub max_speakers: usize }
impl Default for Options { /* language: None, diarize: true, max_speakers: 6 */ }

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SpeechError { Cancelled, NotInstalled(String), UnknownModel(String),
                       Unavailable(String), Model(String), Download(String), Io(String) }

pub type ProgressFn<'a> = &'a mut dyn FnMut(f32, &str) -> bool; // (fraction, status) -> keep going?

pub trait Transcriber: Send + Sync {
    fn id(&self) -> String;
    /// Transcribe mono 16 kHz `audio`; word times are relative to the first sample.
    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn)
        -> Result<Transcript, SpeechError>;
}

/// Returns a fixed transcript, dropping words past the audio length. No model. (Tests/agents.)
pub struct FixedTranscriber { pub transcript: Transcript, pub id: String }

pub fn downmix(channels: &[Vec<f32>]) -> Vec<f32>;               // N channels -> mono
pub fn load(models_dir: &std::path::Path, id: &str)              // feature `whisper`
    -> Result<std::sync::Arc<dyn Transcriber>, SpeechError>;
```
Difference from the donor: the donor returns `filmcraft_project::Transcript` with `filmcraft_time::Tick`
times. Here times are plain `f64` seconds in an inlined `Transcript`/`Word`. Claude converts
`*_sec` → `soundcraft-time` ticks at the engine boundary.

## Features (mirror the donor; both OFF by default)
- `whisper = ["dep:candle-core", "dep:candle-nn"]` — pure-Rust CPU inference.
- `download = ["dep:ureq", "dep:rustls", "dep:rustls-rustcrypto", "dep:sha2"]` — model fetch over HTTPS.
- Default build pulls neither, so a model-less CI compiles and the always-run test passes.

## Layer registration — paste these exact edits into `xtask/src/main.rs`
In `const LAYERS: &[(&str, u8)]`, add the line after `("video", 0),`:
```rust
    ("video", 0),
    ("pa-speech", 0),
    ("dsp", 1),
```
In `const STANDALONE: &[&str]`, add `"pa-speech"`:
```rust
const STANDALONE: &[&str] = &["brand", "time", "audio-io", "midi", "video", "dsp", "pa-speech"];
```
Do **not** add `pa-speech` to `UI_CRATES`, `EXEMPT`, or the `WASM` list (candle isn't a wasm target).
`cargo xtask layers` must still print `layers: ok`.

## Tests — two tiers
1. **No-model (always runs; no model, no committed audio):** synthesize `vec![0.0f32; SAMPLE_RATE as usize * secs]`,
   build a fixture `Transcript` with some words inside the clip and some past its end, run `FixedTranscriber`,
   assert: out-of-range words dropped; remaining `start_sec`/`end_sec` monotonic and within `[0, secs]`; `source == id`.
2. **Real transcription (gated `#![cfg(feature = "whisper")]`):** look for a model under `PA_SPEECH_MODELS_DIR`;
   **SKIP (print + return) if absent — never download in a test**; when present, transcribe a mono-16 kHz `.f32`
   fixture and assert non-empty text. Mirror the donor's `tests/whisper_model.rs`.

## Model
Default id `whisper-base` (`openai/whisper-base` on HuggingFace, MIT). Don't commit weights — `.gitignore`
them, document the download in the crate README.

## Parity — the honest expectation (do not fabricate a +1)
The incumbent menu tree in `crates/engine/catalog/menus.txt` already carries three transcription leaves:
`Setup > Transcription Settings...` and `View > Track Transcription Lane` are **already implemented**
(`setup.transcription_settings`, `view.track_transcription_lane`); `Window > Transcript` is **unimplemented**.
There is **no "Transcribe" menu leaf** — actual speech-to-text is a new capability beyond the incumbent's tree.
So: the `transcribe` engine command + MCP tool add real capability but flip **no** menu-parity leaf on their own
(record 0 for them). Building the **Transcript window** (Claude's UI lane) is what flips `Window > Transcript`
→ +1 at the app level. Record the delta that actually occurs; never invent a +1.

## Lane boundary
Codex edits only `crates/pa-speech/**`, `ATTRIBUTION.md`, the one root-`Cargo.toml` members/deps line, and the
two `xtask/src/main.rs` layer lines. The `transcribe` command + MCP tool, transcript store, and UI are Claude's.

## Addendum — after the hardening pass (what differs from the contract above)
- `SpeechError` has two more variants than listed: `ModelUnavailable(String)` (no searched folder has
  the model; the text names every place looked in) and `TooLong(String)` (audio over
  `MAX_AUDIO_SECS`, 4 hours). `NotInstalled` now reads "is not installed" (nothing downloads a model
  yet).
- `ModelSearch::from_environment(product_folder, xdg_folder)` takes the two application-data folder
  names from the caller's brand. Windows looks in `%LOCALAPPDATA%` first, `%APPDATA%` as the fallback.
- `track.transcribe` reports `status: "ok"` or `status: "model_unavailable"` (with `error` naming every
  place searched, and a `hint`). There is no `no_models_dir` status: every miss reads the same way.
  Both `track.transcribe` and `track.set_transcript` are undoable.
- `Whisper::load` validates the folder's contents (configuration ranges, every token id against the
  vocabulary, alignment heads) before it reads the weights; a bad folder is an `Err`.

## Addendum — after the final SPV (what differs again)
- `SpeechError::ModelUnavailable` is now `{ model, searched }` (the model's catalogue id and the places
  looked in). `SpeechError::summary()` is the one-line, folder-free form for a status line. Messages
  name a model by its catalogue display name (`models::label`), never by its id ("Speech model, base
  (multilingual) is not installed"); the id stays the folder name and the `model` / `source` field of
  engine results.
- A `model_unavailable` result carries `model` (id), `model_name`, `reason` (`not_found`,
  `unsupported_build`, `unknown_model`, `invalid_model`), `why` (one line, no folders) and `error` (the
  full text for agents and logs, with every place searched).
- `track.transcribe` renders the track in 60 s ranges into one mono buffer at the session rate, then
  resamples once; the buffer is capped at `MAX_MONO_SAMPLES` (2^29, about 3.1 h at 48 kHz) and a longer
  track is refused before it is rendered. It never answers `ok` for less than the whole track. The
  track's own end is used, not the session's.
- The menu and the command palette run Transcribe Track as the Transcript window's background job;
  only the control channel and MCP run the command synchronously.
- `Whisper::load` also refuses a tokenizer whose timestamp tokens are not one run of 1501 ids after
  every other token, weights with NaN or infinity (the tensor is named), and de-duplicates the
  alignment heads; decoding stops with an error after 8 windows in a row that advance by under 0.5 s.
