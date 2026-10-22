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
- **Discovery (default build):** `ModelSearch` + `resolve_models_dir` find installed weights at run
  time: `$PA_SPEECH_MODELS_DIR`, then `<bundle>/Contents/Resources/models`, then the user's
  application data folder (see Models). A missing model is `SpeechError::ModelUnavailable`, which
  names every place searched.
- **Tests:** always-run (`tests/no_model.rs`) exercise the seam + `FixedTranscriber`;
  `tests/discovery.rs` covers the search order and the fallbacks with fake (sparse) installs and no
  environment changes. Under `whisper`, the synthetic-voice diarization tests run with no model, and
  `tests/whisper_model.rs` does a real round-trip. It cannot pass without recognising speech: it
  SKIPS (printing `SKIPPED: ...`) only when no model is found and no `$PA_SPEECH_FIXTURE` is set; a
  fixture with no model, an unreadable fixture, or no `$PA_SPEECH_FIXTURE_KEYWORD` fails; every
  word must lie inside the clip. Run it with
  `PA_SPEECH_MODELS_DIR=<dir> PA_SPEECH_FIXTURE=<mono 16 kHz f32 raw> PA_SPEECH_FIXTURE_KEYWORD=<word the clip says> cargo test -p soundcraft-pa-speech --features whisper,download -- --nocapture`
  (optionally `PA_SPEECH_FIXTURE_REFERENCE=<true text>` prints the word error rate). It never downloads.

## Models
Whisper weights (`openai/whisper-*` on Hugging Face, MIT) are downloaded on first use into
`<models_dir>/<id>/` and verified against pinned SHA-256s. **Weights are never bundled or
committed** — see `.gitignore`. Default model: `whisper-base`.

A running program finds the weights in this order (a folder counts only if every file of the model is
there at its catalogue size; the first folder that has it wins):
1. `$PA_SPEECH_MODELS_DIR`;
2. `<app bundle>/Contents/Resources/models` (macOS, from the executable at `Contents/MacOS/<bin>`);
3. the user's application data folder: macOS `~/Library/Application Support/<product name>/models`,
   Windows `%APPDATA%\<product name>\models`, elsewhere `$XDG_DATA_HOME/<folder>/models` or
   `~/.local/share/<folder>/models` (`<folder>` = the product name in lower case without spaces).

The crate does not know the product's name; the caller passes it to `ModelSearch::from_environment`
(the engine passes the brand's name).

## Audio
Transcribers take **mono 16 kHz** `&[f32]`. The caller resamples/downmixes first (`downmix` helps).
