# Pa Tools — Cloud Session Build Prompt (V5, self-contained — resumes after G-18 is built)

*Paste the single block below into a fresh Claude session on claude.ai (keep it on Fable). It is complete on its own: it carries the current repo state, the hard gates, the bootstrap commands, the remaining lanes, and the done-gate, so the new session can continue the Pa Tools build cold without reading any prior chat. Everything it refers to is committed in the repo (`docs/`), so the session reads its contract from files, not from a conversation.*

*What changed since V4: V4 was the prompt to **start** G-18 (speech-to-text) with a Codex assistant porting the donor crate. That is **done** — the `pa-speech` crate (candle Whisper + real diarization + HTTPS downloader), the engine `track.transcribe` command, and the model transcript store are built, verified green, and pushed to `feat/g18-speech` at commit `d366f88`. V5 picks up from there: finish the UI lane, prove the full-workspace build with the `whisper` feature, build the Mac `.app`, then move to the next gap. The Codex review of `d366f88` runs in parallel and comes back as a new task, never as a PR comment.*

---

## ⬇️ PASTE EVERYTHING BELOW THIS LINE INTO THE NEW CLOUD CLAUDE SESSION ⬇️

> You are a Claude cloud session **leading the continued build of Pa Tools**, Gil's Pro Tools-class audio workstation (GR8 ID YUZ.AI) — a public, rebranded fork of `storytold/soundcraft` (license MIT OR Apache-2.0). You own the main task and do the bulk of the work; a desktop session and a Codex assistant help. **Stay on Fable unless Gil changes the model himself.** Use heavy verification (Opus-level SPV, real tests) over quick guesses — get it right once.
>
> ### FIRST, before anything else — load and apply the hard gates
> These are standing law for every action this session takes. Re-read them before each write:
> - **Never overwrite, delete, move, or rename any existing file.** Only ever create new files at new paths. A redo is a new version (V2, V3…) kept beside the old one. If a write would land on an existing path, stop and ask. A move/rename counts as a delete at the source — to relocate, copy and leave the original.
> - **Write only to `4 TB Q`** (`/Volumes/4 TB Q/…`) when a drive is connected. `4 TB P` and `HD 2 TB` are read-only reference. Standing reference libraries (e.g. the AI Style Atlas) are never moved, renamed, or folded into a project — referenced in place.
> - **Announce every folder/file at the moment of creation** with its exact name and full path (e.g. "Making folder: `VIDEOS` at `/Volumes/4 TB Q/…`"). Any change/move to Gil's files is reported immediately, never silent.
> - **No `!` prefix on names.** Plain names, version-labeled (V1/V2/V3). Keep deliverables shallow near the top of the drive; no long nested chains.
> - **No Avid code, no reverse-engineering of Pro Tools.** FilmCraft (`storytold/filmcraft`) is an allowed MIT/Apache clean-room donor. **No third-party product names in the UI.** Brand lives in `crates/brand` only — never rename crates, the `.scraft` extension, or `SOUNDCRAFT_*` env names. Any shipped ffmpeg must be LGPL (never GPL/PATH). `unsafe_code = forbid` stays in force (candle is pure Rust, unaffected).
> - **Never patch a defect — rebuild it from scratch at full quality.** Scrub the Mac username `gilrivera` from anything public.
> - **Keep the sessions in sync:** focus on the main build; have a lower-cost agent handle correspondence (don't burn Fable credits waiting on renders or writing status); contact the desktop session when warranted.
>
> ### Current state — the build is already well underway (read the repo, don't assume)
> - **Repo:** `https://github.com/gilbertriv-ux/pa-tools` (public). Base integration branch: **`pa-tools`** (tip `d1c5e6f`). Active feature branch: **`feat/g18-speech`** (tip **`d366f88`**). Draft tracking **PR #2** (`feat/g18-speech` → `pa-tools`) is open.
> - **G-18 (speech-to-text) is BUILT and pushed on `feat/g18-speech` @ `d366f88`:**
>   - `crates/pa-speech` (package `soundcraft-pa-speech`, standalone **layer 0**) — the public seam (`SAMPLE_RATE = 16_000`, `Word{start_sec,end_sec,text,speaker}`, `Transcript{words,language,source}`, `Options`, `SpeechError`, `ProgressFn`, `Transcriber` trait, `FixedTranscriber`, `downmix`, `word_error_rate`, `load`, `available`). Behind feature **`whisper`**: a full candle 0.9 Whisper port (`whisper/mod.rs` orchestrator, `model.rs` encoder/decoder, `align.rs` DTW word timings, `tokenizer.rs`, `mel.rs` log-mel, `vad.rs`, `diarize.rs` real MFCC + agglomerative clustering). Behind feature **`download`**: a verified streaming HTTPS downloader (`ureq`+`rustls`+`rustls-rustcrypto`+`sha2`). Both features OFF by default. The donor's `filmcraft-time`/`filmcraft-project` types are **inlined** (internal `tick.rs`/`ptranscript.rs`); exact ticks convert to the public `f64` seconds at the `Transcriber` boundary (speaker index → `u8` saturating).
>   - `crates/engine` — a **`track.transcribe`** command: renders the track (`soundcraft_mix::render_clips`), downmixes, resamples to 16 kHz with the anti-aliased Kaiser-sinc path (`soundcraft_dsp::offline::resample`, NOT linear), calls `pa_speech::load`, converts seconds → `Samples` (time origin pinned to session start), stores the result; returns an explicit status (`no_models_dir` / `model_unavailable` / `ok`). The generic MCP layer exposes it for free.
>   - `crates/model` — a transcript store: `Transcript{track,words,language,source}` + `TranscriptWord{start,end,text,speaker}`, `Session.transcripts` with `transcript()`/`set_transcript()`.
>   - **Verification already green at `d366f88`:** `cargo check/clippy/test -p soundcraft-pa-speech --features whisper,download` all pass (9 unit incl. real synthetic-voice diarization + 3 no-model + 1 model test); default clippy/test clean; `cargo xtask layers` → ok; `cargo fmt --check` clean. The real-model test (`transcribes_a_fixture_when_a_model_is_present`) **SKIPS** when no `$PA_SPEECH_MODELS_DIR`/`$PA_SPEECH_FIXTURE` is set (it never downloads) — so end-to-end Whisper transcription is wired and compiles but has not yet been proven against real weights.
> - **The seam/contract lives at `docs/g18-seam.md`;** the remaining-work map at `docs/REMAINING-PHASES-HANDOFF.md` and `docs/gap-queue.md`; architecture/parity at `docs/*.md`. Read `AGENTS.md` and `CLAUDE.md` too.
>
> ### Bootstrap (run first)
> `git clone https://github.com/gilbertriv-ux/pa-tools -b feat/g18-speech && cd pa-tools && git log -1`  → confirm tip is `d366f88`.
> Read, in order: `docs/g18-seam.md`, `docs/REMAINING-PHASES-HANDOFF.md`, `docs/gap-queue.md`, `AGENTS.md`, `CLAUDE.md`, root `Cargo.toml`. If the `4 TB Q` drive is connected, write a claim file `documents/CLAIM-phase-10-g18-ui-<name>-<date>.md` (new path — announce it).
>
> ### Your remaining lanes (in order)
> 1. **UI transcript window — `crates/ui-egui`.** Build the Transcript window that reads `Session.transcripts` and shows words with timecodes and speaker labels, wired to the `track.transcribe` command. This is the one change that flips a real parity leaf: `Window > Transcript` (`window.transcript`) goes unimplemented → **+1**. (`Setup > Transcription Settings` and `View > Track Transcription Lane` are already implemented; the `transcribe` command + MCP tool are a new capability beyond the incumbent's menu tree and flip **no** leaf — record 0 for them. **Never fabricate a +1;** record only the delta that actually occurs.)
> 2. **Full-workspace proof with the `whisper` feature.** The engine depends on `soundcraft-pa-speech` default-features-only, so the default workspace already builds. Additionally prove the featured path end-to-end: build/run with `whisper` (and a downloaded `whisper-base`, `openai/whisper-base`, MIT) so the real-model test actually transcribes instead of skipping — set `$PA_SPEECH_MODELS_DIR` to the weights dir and `$PA_SPEECH_FIXTURE` to a mono 16 kHz LE-`f32` buffer, then `cargo test -p soundcraft-pa-speech --features whisper,download`. Record whether it ran or skipped.
> 3. **Mac `.app` build + live proof.** When green in the cloud, ship to `4 TB Q` and build `Pa Tools.app` via a STEP script, then a live screenshot proving transcription runs in-app. The shipping model is an `openai/whisper-*` weight (MIT), downloaded by the STEP script or bundled under `Resources/` — never committed to git.
> 4. **Next slice (after G-18 merges):** the same clean-room approach for **G-17 (video decode)** in `crates/video` only, FilmCraft donor. Then continue down `docs/gap-queue.md`.
>
> ### Verification bar (before and after every change)
> `cargo fmt --all` · `cargo check --workspace` · `cargo clippy -p soundcraft-pa-speech -p soundcraft-engine -p soundcraft-automation -p soundcraft-ui-egui --all-targets -- -D warnings` · `cargo xtask layers` (pa-speech is standalone L0) · `cargo xtask parity` · `cargo test -p soundcraft-pa-speech -p soundcraft-engine` · `cargo build --release -p soundcraft-cli` then exercise `transcribe` from the CLI and confirm words+timestamps print. Nothing passes on a green check alone: an **Opus SPV that did not write the code** independently reviews the UI wiring and confirms the no-model test is real (FixedTranscriber drops out-of-range words, timings monotonic/in-range), the Whisper round-trip test is `whisper`-gated and SKIP-if-no-model, no silent empty-pass, no GPL/LGPL creep, and the time/transcript types were inlined (not depended on).
>
> ### Git / push notes (these bite — follow exactly)
> - Push commits with author/committer `build@patools.local` (GitHub rejects `gilbertriv@gmail.com` with GH007 email-privacy): `GIT_AUTHOR_EMAIL=build@patools.local GIT_COMMITTER_EMAIL=build@patools.local GIT_AUTHOR_NAME="Pa Tools Build" GIT_COMMITTER_NAME="Pa Tools Build" git commit …`.
> - `gh pr create` fails (GraphQL is blocked: GH007). Open/update PRs via REST: `gh api --method POST repos/gilbertriv-ux/pa-tools/pulls -f title=… -f head=… -f base=… -f body=…`.
> - Do your working edits on `feat/g18-speech` (or a child branch) — never commit working changes onto `pa-tools` directly.
>
> ### Codex coordination
> Codex is independently reviewing/testing `d366f88` (it has Rust 1.95 + clippy). It cannot read PR comments or CI, so any correction it sends arrives to you as relayed text — fold small fixes in yourself at integration; hand Gil a fresh "Codex round 2" task block for anything larger. You are the integrator; Codex does not merge.
>
> ### Done-gate for this phase
> `cargo xtask ci` green · parity delta recorded accurately (expect `Window > Transcript` +1 once the window ships; 0 for the command/MCP tool — never fabricated) · a regression test · SPV recorded · `documents/BUILD-RECORD-phase-10-g18-<date>.md` + `documents/REPORT-phase-10-<name>.md` written to `4 TB Q` (new paths, announced) · Mac screenshot · PR from `feat/g18-speech` into `pa-tools` (REST, as above). Say the truth up front if any step is something only Gil can do (e.g. the Mac build needs the drive + machine connected).

## ⬆️ END OF PASTE ⬆️
