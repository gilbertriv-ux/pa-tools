# Pa Tools — Claude-builds + Codex-assists: paired build prompts (V4, pre-flighted + parity/inline guardrails)

*V4 adds two verified guardrails on top of V3: (1) the inlining of `filmcraft-time`/`filmcraft-project` must extend **throughout** the port (`whisper/mod.rs`, `vad.rs`, `diarize.rs`, not just `lib.rs`) — confirmed the donor has no `filmcraft-audio-io` dep, so `load`/`downmix` are already `f32`-pure; (2) an honest parity note — the matrix already implements `Setup > Transcription Settings` and `View > Track Transcription Lane`, `Window > Transcript` is the only flippable leaf, and the `transcribe` command/MCP tool are a new capability with no leaf, so no `+1` is fabricated. The seam file on `feat/g18-speech` carries both.*

*V3 is built against the **real donor**, not assumptions. I cloned `storytold/filmcraft` and read `crates/speech` directly, so the seam, the layer position, the feature flags and the test design below are verified, not guessed. Changes from V2: donor URL confirmed live; the API seam corrected to the actual `Transcriber` (mono 16 kHz `&[f32]` → `Transcript`); `pa-speech` placed as a **standalone layer-0 crate** (the V2 "just above audio-io" was wrong — `audio-io` is itself layer 0); `filmcraft-time`/`filmcraft-project` must be inlined; the two-tier test pinned to the donor's own `FixedTranscriber`; the truncated run-order step rebuilt as a concrete checklist. I also stood up `feat/g18-speech` with the full seam committed at `docs/g18-seam.md`, so Codex reads it from a file, not a PR comment.*

**Objective:** a Claude cloud session **leads** the build of **G-18 (speech-to-text)**; **Codex assists** on the same build. They share `github.com/gilbertriv-ux/pa-tools` (branch `pa-tools`) and never edit the same files.

**Donor (verified):** `github.com/storytold/filmcraft` → `crates/speech` (package `filmcraft-speech`): the `Transcriber` trait, a `FixedTranscriber`, a Whisper-on-candle engine behind feature `whisper` (off by default), the model catalogue + HTTPS downloader behind feature `download` (off by default). License MIT OR Apache-2.0 — same as this repo, portable. The transcribers take **mono 16 kHz** audio (`const SAMPLE_RATE = 16_000`); the caller resamples/downmixes first (there's a `downmix` helper).

**Lane rule (why they don't collide):**
- **Codex lane** = create the new standalone crate `crates/pa-speech` (port + adapt the donor, pure Rust, two-tier tests). Codex touches **only `crates/pa-speech/**`, `ATTRIBUTION.md`, one line in root `Cargo.toml`, two lines in `xtask/src/main.rs`**. Never `crates/engine`/`model`/`ui-egui`, never the 4 TB Q drive or the Mac.
- **Claude lane** = the branch + the API seam + the engine/model/UI wiring (`transcribe` command + MCP tool, transcript store, transcript view) + the Mac build + SPV + merges.
- **Hand-off:** Codex opens a PR of `crates/pa-speech` into `feat/g18-speech`; Claude reviews, merges, wires it in, builds on the Mac, and PRs into `pa-tools`.

**Shared hard gates:** no Avid code / no reverse-engineering of Pro Tools; FilmCraft is an allowed MIT/Apache clean-room donor; brand lives in `crates/brand` only — never rename crates, the `.scraft` extension, or `SOUNDCRAFT_*` env names; no third-party product names in the UI; `unsafe_code = forbid` outside the three plugin-host crates (candle is pure Rust, so `pa-speech` stays safe).

## Preconditions & known limits — read before firing either prompt
1. **Codex must have `gilbertriv-ux/pa-tools` in its environment with write access** (push a branch + open a PR). The repo is public so cloning needs no auth, but pushing does. **This is the one thing only you can set up** (it's in your Codex/ChatGPT account); everything else below is done.
2. **Donor URL — VERIFIED.** `https://github.com/storytold/filmcraft` resolves and is public (HEAD `74242b5`), with `crates/speech` present. No fallback needed. (If it ever 404s for Codex, the donor would have to be vendored into the repo — but as of now it clones fine.)
3. **`feat/g18-speech` is already pushed**, with the full API seam, the exact `xtask` layer lines, and the test contract committed at **`docs/g18-seam.md`** and mirrored in the PR body. Codex reads the seam from that file (robust — a Codex task can't read PR comments or CI).
4. **Review iteration is manual.** A Codex task can't see PR comments/CI. Claude fixes small things itself at integration; anything larger comes back as a fresh "Codex round 2" task you paste in — never as a comment on the open PR.

---

## PROMPT A — paste into the CLAUDE CLOUD build session

> You are a Claude cloud session **leading the build of gap G-18 (speech-to-text)** for Pa Tools, Gil's Pro Tools-class DAW (a public fork of `storytold/soundcraft`). A **Codex assistant is porting the donor crate in parallel**; you integrate and ship.
>
> **Bootstrap:** `git clone https://github.com/gilbertriv-ux/pa-tools -b feat/g18-speech && cd pa-tools` — the branch already exists with the agreed seam at `docs/g18-seam.md`. Read `docs/g18-seam.md`, `docs/REMAINING-PHASES-HANDOFF.md`, `docs/gap-queue.md` (G-18 row + Engine-lock coordination), `AGENTS.md`, `CLAUDE.md`. If Q is connected, write `documents/CLAIM-phase-10-g18-<name>-<date>.md`.
>
> **Your lane (do NOT build `pa-speech` — Codex owns it):** the seam is fixed in `docs/g18-seam.md` (verified against the donor): a standalone `crates/pa-speech` (package `soundcraft-pa-speech`) exposing a `Transcriber` that takes **mono 16 kHz `&[f32]`** + `Options` + a progress fn and returns a self-contained `Transcript { words: Vec<Word{start_sec,end_sec,text,speaker}>, language, source }`; features `whisper`/`download` off by default. When Codex's crate lands, wire it: a `transcribe` command in `crates/engine` (`cmd!`, doc string) + MCP tool in `crates/automation/src/mcp.rs`; resample/downmix the engine's audio to 16 kHz mono at the call site and convert `start_sec`/`end_sec` → `soundcraft-time` ticks when you store it; a transcript store in `crates/model`; a transcript lane/window in `crates/ui-egui`.
>
> **Verification bar** (before/after): `cargo fmt --all` · `cargo check --workspace` · `cargo clippy -p soundcraft-pa-speech -p soundcraft-engine -p soundcraft-automation -p soundcraft-ui-egui --all-targets -- -D warnings` · `cargo xtask layers` (pa-speech is standalone L0 — see seam) · `cargo xtask parity` · `cargo test -p soundcraft-pa-speech -p soundcraft-engine` · `cargo build --release -p soundcraft-cli` then `pa-tools-cli run --cmd 'transcribe={"path":"<sample.wav>"}'` prints words+timestamps.
> **Parity guardrail (verified):** the matrix already implements `Setup > Transcription Settings` and `View > Track Transcription Lane`; `Window > Transcript` is unimplemented. There is no "Transcribe" menu leaf — the `transcribe` command + MCP tool are a new capability beyond the incumbent's tree and flip **no** leaf (record 0 for them). The one leaf you can flip is `Window > Transcript`, by building the transcript window. Record the delta that actually occurs; if none flips, record 0 and note the new capability in the PR — never fabricate a +1.
>
> **Method:** plan → Sonnet worker(s) for the wiring → an **Opus SPV** independently reviews your wiring and Codex's crate PR → fix, sign off. Nothing on a green check alone. SPV must confirm: the no-model test is real (uses the donor's `FixedTranscriber` on a synthesized buffer + a fixture transcript, asserts out-of-range words dropped and timings monotonic/in-range) and the Whisper round-trip is a separate `whisper`-gated, SKIP-if-no-model test; no silent empty-pass; no GPL/LGPL creep; `filmcraft-time`/`filmcraft-project` were **inlined**, not depended on.
>
> **Mac build:** when green in the cloud, ship to Q and build `Pa Tools.app` via a STEP script, then a live screenshot proving transcription runs. The shipping model is an `openai/whisper-*` weight (MIT) downloaded by the STEP script or bundled in `Resources/`.
>
> **Integrate Codex:** review its PR `G-18: pa-speech crate (FilmCraft port)` into `feat/g18-speech`, run its tests, merge, finish wiring, PR into `pa-tools`. You are the integrator; Codex does not merge. For any required change, fix small ones yourself or hand Gil a "Codex round 2" block for a fresh task — don't leave it as a PR comment.
>
> **Done-gate:** `cargo xtask ci` green · parity delta recorded accurately (per the guardrail above — not fabricated) · regression test · SPV recorded · `documents/BUILD-RECORD-phase-10-g18-<date>.md` + `REPORT-phase-10-<name>.md` on Q · Mac screenshot · PR into `pa-tools`.

---

## PROMPT B — paste into CODEX

> You are Codex **assisting** a Claude session on Pa Tools, a public Rust DAW (fork of `storytold/soundcraft`). Your job is ONE self-contained piece: port FilmCraft's speech-to-text engine into a new standalone crate. **You do not merge; you do not touch any external drive or the Mac; you edit only `crates/pa-speech/**`, `ATTRIBUTION.md`, one line of root `Cargo.toml`, and two lines of `xtask/src/main.rs`.** You cannot read PR comments or CI after you open the PR, so put everything in the PR body; follow-ups arrive as a new task.
>
> **Setup:** need git + Rust 1.95+ (rustup). `git clone https://github.com/gilbertriv-ux/pa-tools -b feat/g18-speech && cd pa-tools && git checkout -b codex/g18-pa-speech`. **Read `docs/g18-seam.md` first — it is the exact API, layer lines, and test contract you build to.** Also read `AGENTS.md`, `CLAUDE.md`, root `Cargo.toml`. Get the donor: `git clone https://github.com/storytold/filmcraft ../filmcraft` (verified live) and read `../filmcraft/crates/speech` in full — `lib.rs` (the `Transcriber` trait, `Options`, `SpeechError`, `FixedTranscriber`, `downmix`, `load`), `models.rs` (catalogue + downloader), `whisper.rs` (candle inference), `vad.rs`, `diarize.rs`, `mel.rs`. MIT/Apache — portable.
>
> **Task — create `crates/pa-speech` (package `soundcraft-pa-speech`, directory `pa-speech`):**
> - Port the donor, but **inline `filmcraft-time` and `filmcraft-project` throughout** — `lib.rs`, `whisper/mod.rs`, `vad.rs` and `diarize.rs` all `use filmcraft_project::…`/`filmcraft_time::…`; replace every one. Do NOT add any `soundcraft-*` or `filmcraft-*` dependency. The donor has **no `filmcraft-audio-io`** dep, so `load`/`downmix` are already `f32`/path-pure — but if any ported code reaches for another workspace type, inline it the same way (times as `f64` seconds). Define your own `pub struct Word { pub start_sec: f64, pub end_sec: f64, pub text: String, pub speaker: u8 }` and `pub struct Transcript { pub words: Vec<Word>, pub language: String, pub source: String }`. Keep the `Transcriber` trait, `Options`, `SpeechError`, `FixedTranscriber`, `downmix`, `SAMPLE_RATE = 16_000` as in the donor.
> - **Features (mirror the donor, both OFF by default):** `whisper = [dep:candle-core, dep:candle-nn]` (pure-Rust inference), `download = [dep:ureq, dep:rustls, …]` (model fetch). Default build pulls neither, so a model-less CI builds. No C-wrapping deps, no GPL/LGPL.
> - **Layer registration — paste the exact lines from `docs/g18-seam.md`:** add `("pa-speech", 0),` to `LAYERS` (right after `("video", 0),`) and add `"pa-speech"` to the `STANDALONE` array. Do NOT add it to `UI_CRATES`, `EXEMPT`, or the `WASM` list (candle isn't a wasm target). `cargo xtask layers` must pass. If the seam file lacks these lines, stop and say so in your PR — don't guess.
> - **Two-tier tests:**
>   - *No-model (always runs, needs no model and no committed audio):* synthesize a silent buffer `vec![0.0f32; SAMPLE_RATE as usize * secs]`, build a fixture `Transcript` with some words inside the clip and some past its end, run it through `FixedTranscriber`, and assert: words past the clip end are dropped, remaining `start_sec`/`end_sec` are monotonic and within `[0, secs]`, and `source == id`. This exercises the real length/ordering logic with zero model.
>   - *Real transcription (gated):* `#![cfg(feature = "whisper")]`; look for a model under an env-pointed dir (e.g. `PA_SPEECH_MODELS_DIR`) and **SKIP (print + return) if absent — never download in the test**; when present, transcribe a mono-16 kHz `.f32` fixture and assert non-empty text. Mirror the donor's `tests/whisper_model.rs`.
> - Default model id `whisper-base` from the catalogue (`openai/whisper-base` on HuggingFace, MIT). Don't commit weights — `.gitignore` them and document the download in the crate README.
> - Add an `ATTRIBUTION.md` row crediting `filmcraft-speech` (MIT/Apache) as the donor.
>
> **Verify and paste into the PR body:** `cargo fmt --all` · `cargo check -p soundcraft-pa-speech` · `cargo clippy -p soundcraft-pa-speech --all-targets -- -D warnings` · `cargo xtask layers` · `cargo test -p soundcraft-pa-speech` (the no-model test runs; the whisper test SKIPs). You don't build the whole app.
>
> **Hand off:** open a PR **into `feat/g18-speech`**, titled **`G-18: pa-speech crate (FilmCraft port)`**, body = what you ported, the exact public API, how you inlined time/project, the feature flags, the license row, and your verification output. **Do not merge.**
>
> **Stay in your lane:** only `crates/pa-speech/**`, `ATTRIBUTION.md`, the one `Cargo.toml` member/deps line, the two `xtask` layer lines. Editing `crates/engine`/`model`/`ui-egui` → stop; that's Claude's.

---

## How to run them
0. **Precondition (yours):** add `gilbertriv-ux/pa-tools` to Codex's GitHub environment with write access.
1. **Done already:** `feat/g18-speech` is pushed with the seam committed at `docs/g18-seam.md`, and a draft tracking PR (#2) is open. A Claude cloud session clones that branch and starts the wiring (Prompt A).
2. **Fire Prompt B in Codex.** It builds `crates/pa-speech` against `docs/g18-seam.md` and PRs into `feat/g18-speech`.
3. Claude reviews + merges Codex's PR, wires it in, runs full verification, Mac build, PR into `pa-tools`. Codex changes come back as a new task, not a PR comment.
4. Second wave: the same pair with **G-18 → G-17** (video decode, `crates/video` only, FilmCraft donor) is the right next parallel slice.
