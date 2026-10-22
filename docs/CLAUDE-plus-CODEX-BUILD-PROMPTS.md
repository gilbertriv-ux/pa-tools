# Pa Tools — Claude-builds + Codex-assists: paired build prompts (V2, risk-hardened)

*V2 adds: the Codex repo precondition and wait-for-branch run order; the exact `xtask` layer-table position published before Codex starts; a two-tier (no-model / model-gated) test contract; and an explicit manual review-relay, since a Codex task can't read its own PR's comments or CI.*

**Objective of this pair:** a Claude cloud session **leads** a build; **Codex assists** on the same build. They share the public repo `github.com/gilbertriv-ux/pa-tools` (branch `pa-tools`) and never edit the same files, so they run truly in parallel and hand off by PR.

**The build:** gap **G-18 — speech-to-text** (the agent-era feature Pro Tools has no answer to). Donor: FilmCraft's `crates/speech` (Whisper on candle, word timestamps), MIT/Apache, available at `github.com/storytold/filmcraft`.

**Lane rule (why they don't collide):**
- **Codex lane** = create a NEW standalone crate `crates/pa-speech` (port + adapt the FilmCraft donor, pure Rust, with unit tests). Codex touches **only `crates/pa-speech`** and `ATTRIBUTION.md`. It never edits `crates/engine`, `model`, or `ui-egui`, never touches the 4 TB Q drive or the Mac.
- **Claude lane** = everything else: the branch, the engine/model/UI wiring (a `transcribe` command + MCP tool, a transcript store, a transcript view), the Mac build + live verification, the SPV review, and the merges.
- **Hand-off:** Codex opens a PR of `crates/pa-speech` into Claude's feature branch `feat/g18-speech`; Claude reviews it, merges it, wires it in, builds on the Mac, and opens the final PR into `pa-tools`.

**Shared hard gates (both agents):** no Avid code and no reverse-engineering of Pro Tools; FilmCraft is an allowed MIT/Apache clean-room donor; brand lives in `crates/brand` only — never rename crates, the `.scraft` extension, or `SOUNDCRAFT_*` env names; no third-party product names in the UI; `unsafe_code = forbid` outside the three plugin-host crates (so `pa-speech` is safe Rust; a C-wrapping dep is out — candle is pure Rust, which is why FilmCraft's port fits).

## Preconditions & known limits — read before firing either prompt
1. **Codex must have `gilbertriv-ux/pa-tools` in its environment first.** The repo is public so a clone needs no auth, but for Codex to push a branch and open a PR its GitHub environment needs write access to the repo (or it will fork). Add the repo to Codex's environment before firing Prompt B. This is the one hard precondition.
2. **Fire Prompt A first and wait for `feat/g18-speech` to be pushed** before firing Prompt B. Prompt B handles a missing branch (push its own branch, open a draft PR against `pa-tools`, wait for Claude to retarget), but having the target branch already up — with the API seam and the exact layer-table position in its description — makes Codex's first version land close to final.
3. **Review iteration is manual.** A Codex Cloud task cannot read PR review comments or CI status from inside the same task — only create a PR. So Claude's review feedback does **not** reach Codex automatically. Two ways to iterate, both in the prompts: Claude fixes small things itself at integration time, or Claude writes a crisp "Codex round 2" instruction block that **you paste into a fresh Codex task**. Don't expect Codex to "see" a comment left on its PR.

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
> - **Set up the seam before Codex starts — Codex builds to exactly what you post, and cannot ask follow-ups mid-task.** Create `feat/g18-speech`, push it, and put in its PR description: (a) the public API you expect — a `Transcriber` that takes PCM/WAV + sample rate and returns `{text, words:[{start,end,text}], language}`, close to FilmCraft's own `speech` API so the port is minimal; (b) **the exact `xtask/src/main.rs` layer-table insertion** — read `xtask/src/main.rs:19-39`, work out where `soundcraft-pa-speech` sits (just above `soundcraft-audio-io`, the crate it depends on, so `cargo xtask layers` passes), and paste the exact line + its two neighbours so Codex places it verbatim instead of guessing; (c) the committed-sample + test contract from the SPV note below.
> - Wire it in once Codex's crate lands: a `transcribe` command in `crates/engine` (`cmd!`, with a doc string) + a matching MCP tool in `crates/automation/src/mcp.rs`; a transcript store in `crates/model`; a minimal transcript lane/window in `crates/ui-egui`.
> - **Verification bar** (record before/after): `cargo fmt --all` · `cargo check --workspace` · `cargo clippy -p soundcraft-pa-speech -p soundcraft-engine -p soundcraft-automation -p soundcraft-ui-egui --all-targets -- -D warnings` · `cargo xtask layers` · `cargo xtask parity` (the Transcription leaf should flip to implemented → parity +1) · `cargo test -p soundcraft-pa-speech -p soundcraft-engine` · `cargo build --release -p soundcraft-cli` then `./target/release/pa-tools-cli run --cmd 'transcribe={"path":"<sample.wav>"}'` prints words+timestamps.
> - **Method:** plan → Sonnet worker(s) for the wiring → an **Opus SPV agent** independently reviews both your wiring and Codex's crate PR → you fix and sign off. Nothing on a green check alone. **SPV must specifically confirm the two-tier test is real:** the no-model test genuinely exercises something (WAV loads → PCM decode → API returns the `{text, words, language}` shape, and on a committed fixture transcript the word timestamps are monotonic and within the clip length) — not an assert-true stub; and the actual Whisper round-trip is a separate test gated behind a feature/env flag because CI has no model. Also check: no silent fallback that returns empty and "passes," no GPL/LGPL creep, the committed WAV is small (a few seconds, mono, low sample rate) and openly licensed.
> - **Iterating with Codex is manual** (a Codex task can't see PR comments): for small issues, just fix them yourself while integrating; for anything substantial, write a tight "Codex round 2" block (what to change, which files, the new expected API) and hand it to Gil to paste into a fresh Codex task. Do not leave the fix as a PR comment and assume Codex will act on it.
> - **Mac build:** when it's green in the cloud, ship it to Q and build `Pa Tools.app` via a STEP script (tar the tree minus `target/`, split ≤15 MB, `device_commit_files` to `build-intel/`, md5-verify, STEP`N`.command untars to a new `source-vN-*/`, builds the Intel target, assembles + signs the `.app`, launches), then a live screenshot proving transcription runs. The shipping Whisper model must be an openly licensed GGUF/safetensors; download it in the STEP script or bundle it in `Resources/` — never a non-free model.
>
> **Integrate Codex's work:** Codex will open a PR titled `G-18: pa-speech crate (FilmCraft port)` into `feat/g18-speech`. Review it (SPV pass + run the crate's tests), request changes with specifics if needed, then merge it into your branch and finish the wiring. **You are the integrator; Codex does not merge.**
>
> **Done-gate:** `cargo xtask ci` green · parity +1 recorded · tests pass incl. a new transcription regression test · SPV review recorded · `documents/BUILD-RECORD-phase-10-g18-<date>.md` + `documents/REPORT-phase-10-<yourname>.md` on Q · the Mac screenshot · a PR from `feat/g18-speech` into `pa-tools` with a one-paragraph summary + the parity delta. **Needs from Gil:** nothing, unless the Mac build needs a click — list it in the report.

---

## PROMPT B — paste into CODEX

> You are Codex **assisting** a Claude session on Pa Tools, Gil's Pro Tools-class DAW — a public Rust repo, a fork of `storytold/soundcraft`. Your job is ONE self-contained piece: port a Whisper speech-to-text engine into a new crate. **You do not merge, you do not touch any external drive or the Mac, you touch only `crates/pa-speech` and `ATTRIBUTION.md`.** Your deliverable is a PR. **You cannot read PR comments or CI results after you open the PR, so put everything (summary, API, license note, verification output) in the PR body; any follow-up will come as a brand-new task, not as a reply on your PR.**
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
> - Add it to `[workspace.dependencies]` and to the **layer table in `xtask/src/main.rs`** at the **exact position the Claude session posted in the `feat/g18-speech` PR description** — paste that line where it says; do not guess the layer. (It sits just above `soundcraft-audio-io`, the crate it depends on; `cargo xtask layers` must pass.) If the PR description has no layer position, stop and note it in your PR rather than guessing.
> - Model download: keep FilmCraft's downloader but default to an **openly licensed** Whisper GGUF/safetensors; make the model path configurable (env or a param) and **do not commit model weights** — add a `.gitignore` line for them and document the download in the crate README.
> - **Unit tests — two tiers, because CI has no model:**
>   - *No-model test (always runs):* commit a short WAV (a few seconds, **mono, low sample rate**, openly licensed or synthesized). Assert the real plumbing: the WAV loads and decodes to PCM at the expected length/rate, the `Transcriber` returns the `{text, words, language}` shape, and against a **committed fixture transcript** the word timestamps are monotonic and within the clip length. This must exercise the decode + API path — not a stub that asserts `true`. If no model is loaded, define a clear behaviour (e.g. return an empty transcript with `Ok`) and test *that* behaviour explicitly; never let "empty because no model" masquerade as a pass.
>   - *Real-transcription test (gated):* the actual Whisper round-trip on the WAV, behind a `#[cfg(feature = "model-tests")]` or an env check, so a model-less CI still builds and passes.
> - Update `ATTRIBUTION.md` with a row crediting FilmCraft's `speech` crate as the donor (MIT/Apache).
>
> **Verify locally and paste the output into your PR:** `cargo fmt --all` · `cargo check -p soundcraft-pa-speech` · `cargo clippy -p soundcraft-pa-speech --all-targets -- -D warnings` · `cargo xtask layers` · `cargo test -p soundcraft-pa-speech` (the no-model tests at least). You do **not** need to build the whole app — the Claude session does the full workspace build + the Mac build.
>
> **Hand off:** open a PR **into the `feat/g18-speech` branch** (not `pa-tools`), titled **`G-18: pa-speech crate (FilmCraft port)`**, with: what you ported, the public API you exposed, the license note, and your verification output. **Do not merge** — the Claude session reviews, runs an independent check, and integrates. If `feat/g18-speech` does not exist yet, push `codex/g18-pa-speech` and open the PR against `pa-tools` as a draft with a comment "retarget to feat/g18-speech when it exists"; the Claude session will retarget.
>
> **Stay in your lane:** edit only `crates/pa-speech/**`, `ATTRIBUTION.md`, `Cargo.toml` (the one `[workspace.dependencies]` line + members), and `xtask/src/main.rs` (the one layer-table line). If you find yourself editing `crates/engine`, `crates/model`, or `crates/ui-egui`, STOP — that's the Claude session's lane.

---

## How to run them
0. **Precondition:** add `gilbertriv-ux/pa-tools` to Codex's GitHub environment (write access, so it can push a branch + open a PR).
1. Fire **PROMPT A** in a Claude cloud session. **Wait for it to push `feat/g18-speech`** with the API seam + the exact layer-table position + the test contract in the PR description.
2. Then fire **PROMPT B** in Codex (it builds `crates/pa-speech` against that seam and PRs into `feat/g18-speech`).
3. Claude reviews + merges Codex's PR, wires it in, builds on the Mac, and PRs into `pa-tools`. If a Codex change is needed, Claude fixes small ones itself or hands you a "Codex round 2" block to paste into a **new** Codex task (Codex can't act on PR comments in the original task).
4. To assist a different gap instead, swap G-18 for an isolated one from `docs/gap-queue.md` (G-17 video decode is the other clean Codex-friendly pick — `crates/video` only) and rename the branches/crate accordingly.
