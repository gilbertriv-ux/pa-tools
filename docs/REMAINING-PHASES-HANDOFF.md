# Pa Tools — Remaining-Phases Handoff V2 (for parallel Claude cloud sessions)

> **V2 supersedes V1 (V1 kept on Q).** What changed: the GitHub repo is now **live**, so sessions clone from GitHub and coordinate by branches + PRs instead of git bundles; preconditions A (repo) and B (gap queue) are both met.

**Purpose:** any fresh Claude cloud session can open this, pick one phase below, and run it end to end on its own credits — without re-discovering the project. Phases 6–10 are independent enough to run in parallel across several sessions. Phase 5 is already in progress in the session that wrote this.

**What Pa Tools is:** Gil's own Pro Tools-class digital audio workstation (GR8 ID YUZ.AI / the PA app family). It is a **fork of the open-source, pure-Rust DAW `storytold/soundcraft`** (MIT OR Apache-2.0), rebranded and extended. Target: ~99% of Avid Pro Tools' workflow, under Gil's brand, with an MCP + CLI agent layer and many music formats. Build order: Intel Mac → Apple silicon → Windows.

---

## CURRENT STATUS (2026-10-22) — read this

- **Repo is LIVE and public:** `https://github.com/gilbertriv-ux/pa-tools`, integration branch **`pa-tools`** @ `1618bf1` (verified-good V2 rebrand base + `docs/gap-queue.md`). Clone with **`git clone https://github.com/gilbertriv-ux/pa-tools -b pa-tools`** — **no auth** (public).
- **Gap queue is in the repo:** `docs/gap-queue.md` (and on Q at `documents/gap-queue.md`). Read your gap's row before claiming.
- **Cleared to fan out now** (the queue's WAVE-0 + stabilizer): **G-18** speech-to-text (new `pa-speech` crate, FilmCraft donor), **G-17** HEVC/DNx/AV1 video decode (FilmCraft, `crates/video`), **G-04** correctness bugs (holds the `crates/engine` lock — G-18/G-17 defer their one-line engine wiring behind G-04's PR per the gap queue's "Engine-lock coordination" section). Do **not** start G-08, G-09, G-12, G-19 until the queue marks them unblocked.
- **Phase 5 (formats)** is in progress in the originating session; it lands as its own PR against `pa-tools`. **Phase 6 (the MCP/CLI agent layer) is the product thesis** — prioritize it alongside or before the Phase 7 alpha blockers, not after.
- **Private mirror:** only *after* the fan-out, `git push --mirror` into a separate private repo. Never flip the public repo's visibility mid-fan-out — it auth-STOPs running sessions.

---

## 0. How a new cloud session bootstraps (read first)

1. **Get the drive.** Everything lives on Gil's external drive **4 TB Q**. In the Claude desktop app on his Mac, use the "+" → **Add folder** → `/Volumes/4 TB Q` (or he shares it). The project root is `/Volumes/4 TB Q/Pa Audio Workstation V1/`.
2. **Read the plan.** `documents/V2-PA-TOOLS-BLUEPRINT.pdf` (the full plan, parity matrix, rebrand map, formats plan) and `documents/V2-PA-TOOLS-MANUSCRIPT.pdf` (everything A–Z). This handoff is the short version for the remaining work.
3. **Get the code — from GitHub (the repo is live).** `git clone https://github.com/gilbertriv-ux/pa-tools -b pa-tools` (public, no auth). Make a feature branch off `pa-tools` for your gap and open a PR into `pa-tools`; never push to the default branch. (Backups also exist on Q: the fork at `source-v2-patools/` and a git bundle at `build-intel/V2-pa-tools-branch-*.bundle` — use these only if GitHub is unreachable.) The untouched V1 baseline is on Q at `source/` — never edit it.
4. **Load the gates.** `AGENTS.md` and `CLAUDE.md` in the repo carry the engineering rules + Gil's hard gates. The non-negotiables: **never overwrite / delete / move** any of Gil's files (only create new files at new paths; a redo is a new V-folder kept alongside the old); **write only to 4 TB Q**; **announce every path** you create; **no Avid code and no reverse-engineering of Pro Tools** (match it from public docs; FilmCraft is an allowed clean-room code donor under the same license); **no third-party product names in the UI**.
5. **Method (superpowers + impeccable):** plan → **lower-model worker agents** (Sonnet) do the build → **Opus supervisor (SPV) agents** independently check every worker's output for errors/omissions → orchestrator (you) fixes and signs off. Nothing ships on a green checkmark alone; verify by building, testing, grepping, and — for the app — a live screenshot on the Mac.
6. **Build tooling.** The cloud container has Rust 1.97 and reaches crates.io; it can `cargo build/test` on Linux but **cannot produce Mac binaries**. Mac builds run **on Gil's Mac** via a `.command` script double-clicked in Finder (Rust lives on Q at `toolchain/`; the build cache at `target-intel/`). Pattern: build + verify in the cloud → `tar czf` the tree (exclude `target/`), split into ≤15 MB parts, `device_commit_files` to `build-intel/`, md5-verify on Q → a STEP`N`.command reassembles, untars to a **new** `source-vN-*/` folder, builds, assembles the `.app`, verifies, launches.
7. **The shipping LGPL ffmpeg** (for the formats sidecar) is already on Q at `/Volumes/4 TB Q/Pa YTube Facts Intel V1/Pa YTube Facts.app/Contents/Resources/bin/ffmpeg` (Intel Mach-O, LGPL, 22 MB). Bundle it into `Pa Tools.app/Contents/Resources/`; never depend on PATH ffmpeg, and never bundle a GPL build.
8. **Coordinate via GitHub + a lightweight lock.** Work on a feature branch and open a **draft PR early** so your work is visible and survives the session ending; mark it ready when the done-gate passes. The lightweight cross-session lock is still a claim file on Q: write `documents/CLAIM-phase-<n>-<gap>-<session>-<date>.md` before starting and check existing CLAIM files against your gap's **Files-touched** column in `docs/gap-queue.md` (collision rule: only one live session may hold `engine`/`mix`/`ui-egui`). A claim older than 24h with no matching `documents/REPORT-*.md` is stale — note it and proceed; never delete another session's claim.

### Key facts to carry
- Brand lives in ONE crate: `crates/brand` (package `soundcraft-brand`). Crate names, the `.scraft` session extension, and `SOUNDCRAFT_*` env names are deliberately unchanged (merge-friendly; invisible to users). Binaries are `pa-tools` and `pa-tools-cli`. Bundle id `com.imagescouture.patools`. MCP server `pa-tools`, resources `patools://…`.
- The engine exposes **480 commands**, the same list from menus/keyboard/CLI/JSON-channel/MCP (17 MCP tools today). Parity: **482/512 app (94.1%), 397/512 engine** — measured by `cargo xtask parity`; a test fails if it drops.
- **FilmCraft** (`/Volumes/4 TB Q/cloned-repos/storytold__filmcraft`, same MIT/Apache license) is the donor for the big gaps: `crates/interchange` + `crates/cfb` (AAF/OMF → G-03), `crates/aac` (AAC encoder → G-21), `crates/speech` (Whisper → G-18), plus FLAC/Opus/AC-3 decoders and Kokoro TTS. SoundCraft's own video crate was already copied from FilmCraft, so this is the sanctioned pattern.
- Verification bar every phase: `cargo fmt` → `cargo check --workspace` → `cargo clippy … -D warnings` (on touched crates) → `cargo xtask layers` → `cargo xtask parity` (record before/after) → targeted `cargo test` → a built artifact → an SPV review → a build record on Q. The disk is capped; delete stray test binaries if you hit "no space".

---

## Phase 5 — Formats  *(STATUS: IN PROGRESS — do not restart)*

A worker is building the `pa-formats` crate in the originating session. **A new session should not run this**; if it has clearly stalled, read `research/phase5-formats-report.md` (on Q once done) and the `pa-tools` branch git log, then continue from there. Scope: MP3/AAC-M4A/Opus/Ogg-Vorbis/ALAC export via the LGPL ffmpeg sidecar; fix the `bounce_stems` silent-WAV fallback; wider imports; stretch = port FilmCraft's native AAC encoder. Done-gate: a round-trip test per format (encode demo → decode → loudness within ~1 LUFS) and a Mac build that bounces MP3/M4A/Opus.

---

## Phase 6 — Agent layer (MCP + CLI Pa extras + secured control channel)

**Goal:** Pa Tools becomes the most agent-controllable DAW there is — the thing that sets it apart from Pro Tools.
**Why:** the engine already exposes every action; Phase 6 adds Pa-family conveniences and closes the one security hole.
**Inputs:** `crates/automation/src/mcp.rs` (the 17-tool MCP server), `apps/soundcraft-cli`, `crates/engine`, the new `pa-formats` crate (Phase 5), `usestrix/strix` on Q (`cloned-repos/usestrix__strix`).
**Tasks (ordered):**
1. **Pa extras** — add commands (and matching MCP tools): `pa.convert` (batch any-format → any-format, via `pa-formats`), `pa.stems` (bounce stems in a chosen format), `pa.loudness_report` (LUFS + true-peak per track to JSON), `pa.session_summary`, and PA-family hand-offs (`pa.export_for_pacut`, `pa.import_paytube_download`). Each is a `cmd!` in `crates/engine` + an MCP tool in `mcp.rs`.
2. **Secure the control channel** — the loopback TCP JSON channel (`apps/soundcraft/src/control_server.rs`, env `PATOOLS_CONTROL_PORT`) has **no authentication** (documented). Add a per-run token (printed once / env), bind 127.0.0.1 only, keep the read/write sandbox roots (`--automation-read-root/-write-root`).
3. **Security audit** — run `strix` against the control channel and the MCP server; record findings; fix anything real.
4. **Verify** — `claude mcp add pa-tools -- pa-tools-cli mcp` connects; `tools/list` shows the new tools; each new tool has a passing test; an unauthenticated control-channel client is rejected.
**Done-gate:** new tools listed + tested; strix pass recorded; auth enforced. **Needs from Gil:** nothing. **Est:** ~8–15 agent-hours (one session).

---

## Phase 7 — Alpha blockers (makes it a *real* Pro Tools replacement)

The three things stopping Pa Tools from being usable for professional work. Each sub-track can be its own parallel session.

**7a — Audio interface routing + duplex low-latency (gap G-01, ~25–40 h).** Today device/rate/buffer selection exists but there's no physical-channel routing and input/output are separate streams. Add I/O Setup channel mapping, one duplex stream on one clock, latency reporting + record-offset compensation, and (owner decision) ASIO behind a build feature — Steinberg's SDK is proprietary/GPLv3, so flag it for Gil. Files: `crates/playback` (cpal), `crates/engine/src/cmd/setup*.rs`. **Needs Gil's audio interface to test.**

**7b — MIDI device input/output (gap G-02, ~20–35 h).** No MIDI hardware layer today. Add **midir** (MIT) devices; realtime input to armed tracks/instruments with timestamps; MIDI record/merge/loop; thru; output tracks; clock out. Files: `crates/midi`, `crates/playback`, `crates/engine`. **Needs a MIDI keyboard to test.**

**7c — AAF/OMF session exchange (gap G-03, ~50–90 h, but FilmCraft donates most of it).** Port FilmCraft `crates/interchange` (aaf/, omf/) + `crates/cfb` into a new `pa-interchange` crate; map the Pro Tools session structure ↔ Pa Tools' session model (`crates/model`). `.ptx` stays out (proprietary, undocumented). Files: new `crates/pa-interchange`, `crates/engine` import/export commands. FilmCraft source: `cloned-repos/storytold__filmcraft`.

**Done-gate (whole phase):** the upstream alpha gate passes on Gil's hardware — record from a chosen interface at a chosen buffer; record MIDI from a keyboard; export an AAF that round-trips into another DAW. **Needs from Gil:** an audio interface, a MIDI keyboard, and his call on ASIO. **Est:** upstream ~110–180 agent-hours total, ~60–70% parallelizable → spread 7a/7b/7c across sessions.

---

## Phase 8 — Apple silicon universal build

**Goal:** one universal (x86_64 + arm64) `Pa Tools.app`, notarized.
**Tasks:** on the Mac, `rustup target add aarch64-apple-darwin`; a STEP script builds both targets and `lipo -create`s them (this is exactly `packaging/macos/package.sh --arch universal`); sign + notarize with Apple Team **QNB6GKU33A** (Gil enters his app-specific password at notarize time). Can be built on the current Intel Mac; tested on the incoming M6.
**Done-gate:** `lipo -info` shows both arches; notarization **Accepted**; runs natively on the M6. **Needs from Gil:** the M6 for final test; his Apple app-specific password. **Est:** ~2–5 agent-hours + the M6.

---

## Phase 9 — Windows MSI via GitHub Actions

**Goal:** x64 + arm64 Windows installers.
**Tasks:** the repo **`gilbertriv-ux/pa-tools` already exists (public)** — public repos get **unlimited free Windows-runner minutes**, so MSI iteration is free. Remaining: vendor the CI logic from `storytold/craft-common-actions` (upstream's workflows call it and it's private — the plan is in `docs/ci-notes-patools.md` in the repo) into a real Windows build workflow; `patools.wxs` already has a fresh UpgradeCode + component GUIDs so it never collides with any SoundCraft install; run the workflow and download the MSI.
**Done-gate:** the MSI installs and runs on a Windows PC. **Needs from Gil:** the public/private decision (to create the repo) and a Windows PC to test. **Est:** ~5–10 agent-hours + a PC.

---

## Phase 10 — Beta → full parity (ongoing, many sessions)

**Goal:** climb from ~57% (essentials) / ~40% (pro) toward ~95% by closing gaps in priority order and merging upstream weekly.
**Method per session:** pick the top unclaimed gap from `docs/gaps.md` (see table below), implement with workers + an SPV review, merge the latest `storytold/soundcraft` upstream (result as a new V-folder on Q; re-run `rebrand.sh` after the merge to re-apply the brand), then `cargo xtask ci` + `cargo xtask parity`, and write a build record on Q. FilmCraft donates: G-17 (HEVC/DNx/AV1 video decode), G-18 (speech-to-text, `crates/speech`), and more.
**Done-gate per session:** the parity number rises; `cargo xtask ci` green; a build record on Q. **Est:** upstream ~350–570 agent-hours to beta, ~950–1,450 to full; heavily parallelizable.

### Gap table (from `docs/gaps.md`; hours are upstream agent-hours)
| Gap | What | Hours | Alpha blocker |
|---|---|---:|---|
| G-01 | Audio interface routing + duplex | 25–40 | ✅ (Phase 7a) |
| G-02 | MIDI device I/O | 20–35 | ✅ (Phase 7b) |
| G-03 | AAF/OMF/.ptx exchange | 50–90 | ✅ (Phase 7c, FilmCraft) |
| G-04 | ~25 correctness bugs | 25–40 | |
| G-05 | macOS VST3 entitlement + AU editors | 15–25 | partial (entitlement already fixed in V2) |
| G-06 | Record-to-disk streaming (creek) | 15–25 | partial |
| G-08 | Thin built-in plugin catalogue | 80–120 | |
| G-09 | Localization (CJK/RTL) | 70–110 | |
| G-12 | Dolby Atmos / ADM / MPEG-H | 50–80 | |
| G-18 | Speech-to-text (FilmCraft `speech`) | 30–50 | |
| G-19 | UI feel / pro-grade meters | 60–100 | |
| G-21 | Compressed export | 10–20 | Phase 5 |
| (others G-07,10,11,13–17,20,22) | see docs/gaps.md | | |

---

## Appendix A — paths & commands

- Project root (Q): `/Volumes/4 TB Q/Pa Audio Workstation V1/` → `source-v2-patools/` (fork), `source/` (V1 baseline), `build-intel/` (STEP scripts, logs, apps, git bundle), `documents/` (blueprint, manuscript, build records, research, SPV), `cloned originals/`, `toolchain/`, `target-intel/`.
- Donor + tool repos (Q): `/Volumes/4 TB Q/cloned-repos/` → `storytold__filmcraft`, `storytold__soundcraft`, `storytold__craft-libs`, `morluto__rea`, `usestrix__strix`, `kyegomez__swarms`, `huggingface__smolagents`, `panniantong__agent-reach`, `D4Vinci__Scrapling`, and more.
- Shipping LGPL ffmpeg (Q): `Pa YTube Facts Intel V1/Pa YTube Facts.app/Contents/Resources/bin/ffmpeg` (+ ffprobe).
- Build on the Mac: double-click a `build-intel/STEP*.command` in Finder; read its `STEP*-LOG-*.txt`.
- Verify loop (cloud): `cargo fmt --all` · `cargo check --workspace` · `cargo clippy -p <touched> --all-targets -- -D warnings` · `cargo xtask layers` · `cargo xtask parity` · `cargo test -p <touched>` · `cargo build --release -p soundcraft-cli` then `./target/release/pa-tools-cli --version`.
- Impeccable (design quality) at `/Volumes/4 TB Q/EXTRA/1/impeccable`; superpowers at `/Volumes/4 TB Q/GitHub Repos/Batch-Oct9/superpowers`.

## Appendix B — decisions still pending from Gil (none block starting a phase)
1. **GitHub public or private** → then the repo is created and all sessions coordinate via it (unblocks Phase 9).
2. **Rename "AudioSuite", "Beat Detective", "Elastic Audio"?** (still Avid feature vocabulary in the menus.)
3. **Homepage URL** (`gr8idyuz.ai` is a placeholder shown in About and sent to plugins as the host URL).
4. **Final app icon** via ChatGPT (current gold-"Pa"/waveform mark is an original placeholder).
5. (Decided) MP3 export → LGPL FFmpeg sidecar. (Decided) bundle id → `com.imagescouture.patools`.

## Appendix C — coordination protocol for parallel sessions
- Claim a gap: write `documents/CLAIM-phase-<n>-<gap>-<yourname>-<date>.md` before starting; check it against every live CLAIM and the gap queue's Files-touched column.
- Work on a feature branch off `pa-tools`; open a **draft PR early**; mark ready at the done-gate.
- Report: write `documents/REPORT-phase-<n>-<yourname>.md` when done, with the parity delta and the PR link.
- Never edit another session's in-progress files; never touch `source/` (V1); always new folders/new files.
- The repo is live: push your feature branch and open a PR into `pa-tools`. Bundles on Q are backup only.
