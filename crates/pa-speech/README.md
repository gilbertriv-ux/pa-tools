# soundcraft-pa-speech

Speech-to-text for Pa Tools (Window ▸ Transcript). Standalone, layer-0 crate — no `soundcraft-*`
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
  environment changes. Under `whisper`: the numeric unit tests (mel, DTW, level detection, ticks,
  diarization caps and cancel), `tests/hostile_models.rs` (model folders of the right size with
  hostile content, which `load` must refuse with an error), and `tests/whisper_model.rs`, a real
  round-trip that **skips exactly when `$PA_SPEECH_FIXTURE` is unset**, whether or not a model is
  installed. A skip proves nothing about speech: it prints `SKIPPED: <reason>` to stdout and stderr,
  and `cargo test` hides the output of a passing test, so use `-- --nocapture` to see it. With the
  fixture set it never skips: a fixture with no model found, an unreadable fixture, or no
  `$PA_SPEECH_FIXTURE_KEYWORD` fails; the transcript must contain the keyword as a whole word
  (case-insensitive, punctuation stripped; `coun` does not match `country`); every word must lie
  inside the clip, end no earlier than it starts, and neither start before nor overlap the word
  ahead of it; and with `PA_SPEECH_FIXTURE_REFERENCE=<true text>` the word error rate must be below
  0.25. Run it with
  `PA_SPEECH_MODELS_DIR=<dir> PA_SPEECH_FIXTURE=<mono 16 kHz f32 raw> PA_SPEECH_FIXTURE_KEYWORD=<word the clip says> cargo test -p soundcraft-pa-speech --features whisper,download -- --nocapture`.
  It never downloads. CI builds and tests the crate with those features, but without a fixture, so
  the real-model test skips there.

## Models
Whisper weights (`openai/whisper-*` on Hugging Face, MIT) are listed in the catalogue with pinned
SHA-256s; the `download` feature has a verified downloader that fills `<models_dir>/<id>/`, but no
program calls it yet. **Weights are never bundled or committed** — see `.gitignore`. Default model:
`whisper-base`. The catalogue's display names are neutral ("Speech model, base (multilingual)");
the author, licence and source fields keep the attribution.

A running program finds the weights in this order (a folder counts only if every file of the model is
there at its catalogue size; the first folder that has it wins):
1. `$PA_SPEECH_MODELS_DIR`;
2. `<app bundle>/Contents/Resources/models` (macOS, from the executable at `Contents/MacOS/<bin>`);
3. the user's application data folder: macOS `~/Library/Application Support/<product folder>/models`,
   Windows `%LOCALAPPDATA%\<product folder>\models` (else `%APPDATA%\<product folder>\models`),
   elsewhere `$XDG_DATA_HOME/<xdg folder>/models` or `~/.local/share/<xdg folder>/models`.

The crate does not know the product's names; the caller passes both folder names to
`ModelSearch::from_environment(product_folder, xdg_folder)` (the engine passes the brand's
`CONFIG_DIR_NAME` and `CONFIG_DIR_XDG`, so the models sit beside the settings).

Nothing in the app downloads a model yet: put the model's folder (`whisper-base`, with its four
files) in one of those places. A running program refuses a folder whose files are damaged or
implausible (`load` validates the configuration, the tokenizer and every token id before it reads
the weights).

## Audio
Transcribers take **mono 16 kHz** `&[f32]`. The caller resamples/downmixes first (`downmix` helps).
