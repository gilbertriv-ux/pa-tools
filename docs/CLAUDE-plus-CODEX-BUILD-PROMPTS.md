# Pa Tools — Claude-builds + Codex-assists: paired build prompts (V1)

**Objective of this pair:** a Claude cloud session **leads** a build; **Codex assists** on the same build. They share the public repo `github.com/gilbertriv-ux/pa-tools` (branch `pa-tools`) and never edit the same files, so they run truly in parallel and hand off by PR.

**The build:** gap **G-18 — speech-to-text** (the agent-era feature Pro Tools has no answer to). Donor: FilmCraft's `crates/speech` (Whisper on candle, word timestamps), MIT/Apache, available at `github.com/storytold/filmcraft`.

**Lane rule (why they don't collide):**
- **Codex lane** = create a NEW standalone crate `crates/pa-speech` (port + adapt the FilmCraft donor, pure Rust, with unit tests). Codex touches **only `crates/pa-speech`** and `ATTRIBUTION.md`. It never edits `crates/engine`, `model`, or `ui-egui`, never touches the 4 TB Q drive or the Mac.
- **Claude lane** = everything else: the branch, the engine/model/UI wiring (a `transcribe` command + MCP tool, a transcript store, a transcript view), the Mac build + live verification, the SPV review, and the merges.
- **Hand-off:** Codex opens a PR of `crates/pa-speech` into Claude's feature branch `feat/g18-speech`; Claude reviews it, merges it, wires it in, builds on the Mac, and opens the final PR into `pa-tools`.

**Shared hard gates (both agents):** no Avid code and no reverse-engineering of Pro Tools; FilmCraft is an allowed MIT/Apache clean-room donor; brand lives in `crates/brand` only — never rename crates, the `.scraft` extension, or `SOUNDCRAFT_*` env names; no third-party product names in the UI; `unsafe_code = forbid` outside the three plugin-host crates (so `pa-speech` is safe Rust; a C-wrapping dep is out — candle is pure Rust, which is why FilmCraft's port fits).

---

## PROMPT A — paste into the CLAUDE CLOUD build session

> You are a Claude cloud session **leading the build of gap G-18 (speech-to-text)** for Pa Tools, Gil's Pro Tools-class DAW (a public fork of `storytold/soundcraft`). A **Codex assistant is working in parallel** on the self-contained donor crate; you integrate its work and ship.
>
> **Bootstrap (in order):**
> 1. `git clone https://github.com/gilbertriv-ux/pa-tools -b pa-tools && cd pa-tools` (public, no auth). Create your feature branch: `git checkout -b feat/g18-speech`.
> 2. Read `docs/REMAINING-PHASES-HANDOFF.md`, `docs/gap-queue.md` (the **G-18 row** and the **Engine-lock coordination** section), and `AGENTS.md` + `CLAUDE.md` (hard gates).
> 3. If the 4 TB Q drive is connected, write `documents/CLAIM-phase-10-g18-<yourname>-<date>.md` and check existing CLAIM files per the collision rule. Your gap touches `crates/engine`, `model`, `ui-egui` for wiring — if a live G-04 claim holds the engine lock, keep your wiring commit for last and rebase onto G-04's merged PR (per the gap queue).
>
> **Your lane (do NOT build the `pa-speech` crate itself — Codex owns that):**
> - Define the integration seam first so Codex can build against it: in `feat/g18-speech`, add a thin `crates/pa-speech`-facing contract note in your PR description (the public API you expect: a `Transcriber` that takes PCM/WAV + sample rate and returns `{text, words:[{start,end,text}], language}`). Keep it close to FilmCraft's own `speech` API so the port is minimal.
> - Wire it in once Codex's crate lands: a `transcribe` command in `crates/engine` (`cmd!`, with a doc string) + a matching MCP tool in `crates/automation/src/mcp.rs`; a transcript store in `crates/model`; a minimal transcript lane/window in `crates/ui-egui`.
> - **Verification bar** (record before/after): `cargo fmt --all` · `cargo check --workspace` · `cargo clippy -p soundcraft-pa-speech -p soundcraft-engine -p soundcraft-automation -p soundcraft-ui-egui --all-targets -- -D warnings` · `cargo xtask layers` · `cargo xtask parity` (the Transcription leaf should flip to implemented → parity +1) · `cargo test -p soundcraft-pa-speech -p soundcraft-engine` · `cargo build --release -p soundcraft-cli` then `./target/release/pa-tools-cli run --cmd 'transcribe={"path":"<sample.wav>"}'` prints words+timestamps.
> - **Method:** plan → Sonnet worker(s) for the wiring → an **Opus SPV agent** independently reviews both your wiring and Codex's crate PR (no silent fallbacks, no GPL creep, tests real) → you fix and sign off. Nothing on a green check alone.
> - **Mac build:** when it's green in the cloud, ship it to Q and build `Pa Tools.app` via a STEP script (tar the tree minus `target/`, split ≤15 MB, `device_commit_files` to `build-intel/`, md5-verify, STEP`N`.command untars to a new `source-vN-*/`, builds the Intel target, assembles + signs the `.app`, launches), then a live screenshot proving transcription runs. The shipping Whisper model must be an openly licensed GGUF/safetensors; download it in the STEP script or bundle it in `Resources/` — never a non-free model.
>
> **Integrate Codex's work:** Codex will open a PR titled `G-18: pa-speech crate (FilmCraft port)` into `feat/g18-speech`. Review it (SPV pass + run the crate's tests), request changes with specifics if needed, then merge it into your branch and finish the wiring. **You are the integrator; Codex does not merge.**
>
> **Done-gate:** `cargo xtask ci` green · parity +1 recorded · tests pass incl. a new transcription regression test · SPV review recorded · `documents/BUILD-RECORD-phase-10-g18-<date>.md` + `documents/REPORT-phase-10-<yourname>.md` on Q · the Mac screenshot · a PR from `feat/g18-speech` into `pa-tools` with a one-paragraph summary + the parity delta. **Needs from Gil:** nothing, unless the Mac build needs a click — list it in the report.

---

## PROMPT B — paste into CODEX

> You are Codex **assisting** a Claude session on Pa Tools, Gil's Pro Tools-class DAW — a public Rust repo, a fork of `storytold/soundcraft`. Your job is ONE self-contained piece: port a Whisper speech-to-text engine into a new crate. **You do not merge, you do not touch any external drive or the Mac, you touch only `crates/pa-speech` and `ATTRIBUTION.md`.** Your deliverable is a PR.
>
> **Setup:**
> 1. Prereqs: `git`, Rust **1.95+** via rustup. Verify `cargo --version`.
> 2. `git clone https://github.com/gilbertriv-ux/pa-tools` → `cd pa-tools` → `git fetch origin pa-tools` → `git checkout pa-tools` → `git checkout -b codex/g18-pa-speech`.
> 3. Read `docs/gap-queue.md` (the **G-18** row), `AGENTS.md`, `CLAUDE.md` (hard gates), and the root `Cargo.toml` (workspace layout; note `[workspace.package]`, the `[workspace.dependencies]` table, and that crates keep the `soundcraft-` package-name prefix).
> 4. Get the donor: `git clone https://github.com/storytold/filmcraft ../filmcraft` and read `filmcraft/crates/speech` in full (the `Transcriber` trait, the Whisper model catalogue + downloader, the candle inference path, word timestamps, speaker labelling). It is MIT OR Apache-2.0 — the same license as this repo, so it may be ported.
>
> **Your task — create `crates/pa-speech` (package `soundcraft-pa-speech`):**
> - Port and adapt FilmCraft's `speech` crate into `crates/pa-speech`: a `Transcriber` whose public API takes PCM or a WAV path + sample rate and returns `{ text, words: [{start, end, text}], language }` (keep it close to FilmCraft's API so the Claude session can wire it with minimal glue; the Claude session's PR states the exact seam it expects — match it).
> - **Pure Rust, no `unsafe` in this crate** (the workspace sets `unsafe_code = forbid` outside the plugin hosts; candle is pure Rust, so this holds). No C-wrapping deps. Do not pull GPL/AGPL/LGPL code.
> - Add it to `[workspace.dependencies]` and to the **layer table in `xtask/src/main.rs`** at the correct layer (it depends on `soundcraft-audio-io` for PCM types, so it sits just above it — read `xtask/src/main.rs:19-39` first; `cargo xtask layers` must still pass).
> - Model download: keep FilmCraft's downloader but default to an **openly licensed** Whisper GGUF/safetensors; make the model path configurable (env or a param) and **do not commit model weights** — add a `.gitignore` line for them and document the download in the crate README.
> - **Unit tests** in the crate: a committed short sample WAV (a few seconds of speech, openly licensed or synthesized) → assert the transcript is non-empty and word timestamps are monotonic and within the clip length. Gate the model-download test behind a feature or an env check so CI without the model still builds.
> - Update `ATTRIBUTION.md` with a row crediting FilmCraft's `speech` crate as the donor (MIT/Apache).
>
> **Verify locally and paste the output into your PR:** `cargo fmt --all` · `cargo check -p soundcraft-pa-speech` · `cargo clippy -p soundcraft-pa-speech --all-targets -- -D warnings` · `cargo xtask layers` · `cargo test -p soundcraft-pa-speech` (the no-model tests at least). You do **not** need to build the whole app — the Claude session does the full workspace build + the Mac build.
>
> **Hand off:** open a PR **into the `feat/g18-speech` branch** (not `pa-tools`), titled **`G-18: pa-speech crate (FilmCraft port)`**, with: what you ported, the public API you exposed, the license note, and your verification output. **Do not merge** — the Claude session reviews, runs an independent check, and integrates. If `feat/g18-speech` does not exist yet, push `codex/g18-pa-speech` and open the PR against `pa-tools` as a draft with a comment "retarget to feat/g18-speech when it exists"; the Claude session will retarget.
>
> **Stay in your lane:** edit only `crates/pa-speech/**`, `ATTRIBUTION.md`, `Cargo.toml` (the one `[workspace.dependencies]` line + members), and `xtask/src/main.rs` (the one layer-table line). If you find yourself editing `crates/engine`, `crates/model`, or `crates/ui-egui`, STOP — that's the Claude session's lane.

---

## How to run them
1. Fire **PROMPT A** in a Claude cloud session first (it creates `feat/g18-speech` and posts the API seam).
2. Fire **PROMPT B** in Codex (it builds `crates/pa-speech` and PRs into that branch).
3. Claude reviews + merges Codex's PR, wires it in, builds on the Mac, and PRs into `pa-tools`.
4. To assist a different gap instead, swap G-18 for an isolated one from `docs/gap-queue.md` (G-17 video decode is the other clean Codex-friendly pick — `crates/video` only) and rename the branches/crate accordingly.
