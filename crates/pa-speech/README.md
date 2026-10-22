# soundcraft-pa-speech

Speech-to-text for Pa Tools (Text ▸ Transcript). Standalone, layer-0 crate — no `soundcraft-*`
dependency. Ported from FilmCraft's [`filmcraft-speech`](https://github.com/storytold/filmcraft)
(MIT OR Apache-2.0); the donor's `filmcraft-time` and `filmcraft-project` types are inlined here as
plain `f64` seconds.

## Status
- **Skeleton (this commit):** the seam — `Transcriber`, `FixedTranscriber`, `Transcript`/`Word`,
  `Options`, `SpeechError`, `downmix`, `word_error_rate`, `SAMPLE_RATE = 16_000`, the pinned model
  `catalogue`, and `load` (gated) — all compiling, with always-run tests and a green
  `cargo xtask layers`.
- **Flesh-out (feature `whisper`):** port the donor's `whisper/` module (candle inference + DTW word
  timestamps), `vad.rs`, `diarize.rs`, `mel.rs`, inlining the time/project types throughout; add the
  candle deps to the `whisper` feature. See `docs/g18-seam.md`.
- **Flesh-out (feature `download`):** port the donor's verified streaming downloader; add
  `ureq` + `rustls` + `rustls-rustcrypto` + `sha2` to the `download` feature.

## Models
Whisper weights (`openai/whisper-*` on Hugging Face, MIT) are downloaded on first use into
`<models_dir>/<id>/` and verified against pinned SHA-256s. **Weights are never bundled or
committed** — see `.gitignore`. Default model: `whisper-base`.

## Audio
Transcribers take **mono 16 kHz** `&[f32]`. The caller resamples/downmixes first (`downmix` helps).
