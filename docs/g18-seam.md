# G-18 seam — `crates/pa-speech` (authoritative contract for the Codex port)

This file is the exact contract the Codex assistant builds to. It was derived by reading the
donor `storytold/filmcraft` `crates/speech` (package `filmcraft-speech`) directly, not from memory.
Claude wires the engine/model/UI around this seam; Codex builds the crate to match it.

## Crate identity
- Directory `crates/pa-speech`, package **`soundcraft-pa-speech`**, short name `pa-speech`.
- **Standalone, layer 0.** No `soundcraft-*` or `filmcraft-*` dependency. (The donor depends on
  `filmcraft-time` and `filmcraft-project`; both are **inlined** here — see types below.)
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

## Lane boundary
Codex edits only `crates/pa-speech/**`, `ATTRIBUTION.md`, the one root-`Cargo.toml` members/deps line, and the
two `xtask/src/main.rs` layer lines. The `transcribe` command + MCP tool, transcript store, and UI are Claude's.
