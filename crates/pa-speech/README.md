# soundcraft-pa-speech

Speech-to-text for Pa Tools (Text ▸ Transcript). Standalone, layer-0 crate — no `soundcraft-*`
dependency. Ported from FilmCraft's [`filmcraft-speech`](https://github.com/storytold/filmcraft)
(MIT OR Apache-2.0); the donor's `filmcraft-time` and `filmcraft-project` types are inlined here as
plain `f64` seconds.

## Status
- **Seam (default build):** `Transcriber`, `FixedTranscriber`, `Transcript`/`Word`, `Options`,
  `SpeechError`, `downmix`, `word_error_rate`, `SAMPLE_RATE = 16_000`, the pinned model `catalogue`,
  and `load` (gated) — no candle dependency, with always-run tests and a green `cargo xtask layers`.
- **Feature `whisper`:** candle Whisper inference — `whisper/` (encoder/decoder, greedy timestamped
  decoding, DTW word timestamps via `align`), `mel` (log-mel front end), `vad` (energy-based bound
  tightening), `diarize` (MFCC + agglomerative speaker clustering). Internally exact ticks
  (`tick`/`ptranscript`), converted to the public `f64` seconds at the `Transcriber` boundary. The
  donor's `filmcraft-time`/`filmcraft-project` types are inlined; no external workspace dep.
- **Feature `download`:** the verified streaming model downloader (`ureq` + `rustls` +
  `rustls-rustcrypto` + `sha2`), SHA-256-checked, pure-Rust TLS.
- **Tests:** always-run (`tests/no_model.rs`) exercise the seam + `FixedTranscriber`; under `whisper`,
  the synthetic-voice diarization tests run with no model, and `tests/whisper_model.rs` does a real
  round-trip when `$PA_SPEECH_MODELS_DIR` + `$PA_SPEECH_FIXTURE` are present (SKIP otherwise, never
  downloads). Run them with `cargo test -p soundcraft-pa-speech --features whisper,download`.

## Models
Whisper weights (`openai/whisper-*` on Hugging Face, MIT) are downloaded on first use into
`<models_dir>/<id>/` and verified against pinned SHA-256s. **Weights are never bundled or
committed** — see `.gitignore`. Default model: `whisper-base`.

## Audio
Transcribers take **mono 16 kHz** `&[f32]`. The caller resamples/downmixes first (`downmix` helps).
