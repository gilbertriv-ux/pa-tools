# Pa Tools — Gap Queue (dependency-ordered decomposition of Phase 10)

**What this is:** Phase 10 ("beta → parity") is not one phase — it is 15+ gaps of very different risk and ~350–1,450 agent-hours. This file is the dependency graph so N parallel cloud sessions don't thrash. **A session must not claim a gap whose `Blocked-by` is unmet, and must not start if any active `CLAIM-*.md` already holds a crate in this gap's `Files touched` (except the isolated new-crate work).** Canonical copy lives at `docs/gap-queue.md` in the repo; this copy on Q is for sessions reading before they clone.

**Collision rule (hard):** at most one active session may hold `crates/engine`, `crates/mix`, or `crates/ui-egui` at a time. The only exceptions are gaps that live almost entirely in a **new or isolated crate** (G-18 → new `pa-speech`; G-17 → `crates/video`), which may run alongside an engine-holding session because their wiring surface is tiny. Check `documents/CLAIM-*.md` before starting.

**Definition of done (every gap):** `cargo xtask ci` green · `cargo xtask parity` recorded before/after (rose, or held with a stated reason) · targeted tests pass · a **new regression test** for the gap · SPV review recorded · build record + report on Q · PR opened against `pa-tools`.


## Engine-lock coordination (G-04 vs the WAVE-0 donor gaps) — read before claiming
G-04 holds the `crates/engine` lock while its claim is live. G-18 and G-17 run **in parallel** with it — but only their isolated crate work (`pa-speech`, the video-codec crate) and tests, which do not touch `crates/engine`. Their small engine/model/ui wiring is **deferred behind G-04's PR**. Mechanics (both are mandatory, so the deferral is a real artifact, not a promise in one session's memory):
1. **Open your PR early as a draft** with the new crate + tests, and a top comment: "engine wiring deferred behind G-04's PR — see gap-queue.md." The work is then visible, reviewable, and survives the session ending.
2. When G-04's PR merges, **rebase onto it and add the wiring as the final commit** of your PR, then mark it ready.
3. **Do not edit `crates/engine` while G-04's claim is live — unless that claim is older than 24h with no matching `REPORT-phase-10-*.md`**, in which case treat the engine lock as released, add your wiring commit, and note the stale-lock release in your PR. (Same 24h staleness rule as claiming a gap — prevents a deadlock behind a crashed G-04 session.)

---

## WAVE 0 — start now, in parallel, the moment the repo exists (isolated · donor-rich · hardware-free)

| Gap | What | Donor | Files touched (collision surface) | Blocked-by | DoD / parity delta |
|---|---|---|---|---|---|
| **G-18** | Speech-to-text transcription | **FilmCraft `crates/speech`** (Whisper on candle, word timestamps) | **new crate `pa-speech`**; thin wiring: `crates/engine` (1 `transcribe` command + MCP tool), `crates/model` (transcript store), `crates/ui-egui` (transcript lane/window) | none | transcribe a fixed offline WAV → words+timestamps; `transcribe` command + MCP tool; parity **+1** (the Transcription leaf becomes implemented); regression test on a committed clip |
| **G-17** | HEVC / DNx / AV1 video decode | **FilmCraft** video codec crates | **`crates/video` only** (+ maybe one engine import line) | none | decode a sample of each codec to frames; import breadth up; regression test per codec; parity held |
| **G-21** | Compressed export (MP3/AAC/Opus/Vorbis/ALAC) | — (this is **Phase 5**) | `pa-formats`, `crates/engine` (already Phase 5's) | **Phase 5 merge** | once Phase 5 lands this is a **verification/regression** session, not new code: add a round-trip test per format, confirm parity + loudness |

These three can run in three separate cloud sessions at once without colliding: G-18 and G-17 live in their own crates, and G-21 is verification of already-merged work.

## WAVE 1 — foundational; run before the interaction-heavy gaps

| Gap | What | Donor | Files touched | Blocked-by | Note |
|---|---|---|---|---|---|
| **G-04** | ~25 correctness bugs in "done" features | — | `crates/engine`, `mix`, `playback`, `midi`, import, CLI, MCP, web, `ui-egui` (**broad**) | none | **Blocks** clean work on G-08/G-12/G-16/G-19 — they build on these surfaces. Run as a focused session with **no other engine/mix session concurrent**. DoD: each issue in `docs/gaps.md` G-04 closed with a regression test |
| **G-05** | AU editor views (VST3 entitlement already fixed in V2) | — | `crates/au-host`, `vst3-host`, `packaging/macos` | none | small; **needs a Mac** for the final plugin-load proof (hardware-free slice: build + unit test the editor-view host code; prove on the Mac) |
| **G-06** | Record / bounce streamed to disk (not whole-file in RAM) | **creek** (MIT) | `crates/playback`, `model` (SourcePool), `engine` | none | hardware-free slice: stream a long bounce to disk tested with the demo song; the interface part is Phase 7a |

## WAVE 2 — after G-04; serialize among themselves (shared mixer/engine/UI surface)

| Gap | What | Donor | Files touched | Blocked-by |
|---|---|---|---|---|
| **G-08** | Deeper built-in plugin catalogue | — (original DSP) | `crates/dsp/src/plugins/*`, `ui-egui` panels | G-04; conflicts with G-19 |
| **G-16** | Session templates, track presets, workspace browser, batch rename | — | `crates/engine` (io, commands), `model`, `ui-egui` | G-04 |
| **G-12** | Dolby Atmos / ADM BWF / MPEG-H (software renderer) | — (clean-room renderer) | `crates/mix` (panner, renderer), `model`, `engine`, `ui-egui` | G-04; conflicts with any mix session |
| **G-15** | Elastic Pitch + per-algorithm choice | — | `crates/dsp`, `engine` | G-04 |
| **G-20** | MIDI depth: learn, sampler polish, notation editing | — | `crates/midi`, `dsp`, `ui-egui` | G-04; MIDI-learn also needs **G-02** (Phase 7b) |

## WAVE 3 — run solo, late (highest collision / needs breadth)

| Gap | What | Files touched | Why last |
|---|---|---|---|
| **G-09** | Localization: string catalog, CJK/RTL rendering | **every crate** (the whole string surface) | touches the same strings as the brand work; must run with no other session editing strings |
| **G-19** | UI feel: pro-grade meters, scaling, plugin windows, shortcut collisions | `crates/ui-egui` (broad) | conflicts with every UI gap; needs visual judgment + Mac screenshots |

## Phase 7 hardware gaps (here for the graph; not Phase 10)

| Gap | Needs | Hardware-free slice buildable now |
|---|---|---|
| **G-01** audio interface routing + duplex | an interface | I/O Setup channel-mapping, latency reporting, record-offset — built + unit-tested against the cpal device list and a **virtual loopback** (BlackHole / CoreAudio aggregate); only duplex-on-one-clock waits for Gil's gear |
| **G-02** MIDI device I/O | a MIDI keyboard | `midir` enumeration + a **virtual MIDI port** (macOS IAC bus) + the record/merge/loop state machine — fully testable without hardware |
| **G-10** control surfaces (HUI/EUCON) | a surface | protocol layer; depends on **G-02** |
| **G-11** external sync (MTC/LTC/clock) | sync source | depends on **G-01/G-02** |
| **G-13** sidechain / ARA / out-of-process sandboxing | — | host-crate work; mostly hardware-free |

---

## Recommended first fan-out (when the repo exists)
- **Session A:** G-18 speech-to-text (new `pa-speech` crate; FilmCraft donor). Highest "it's a feature Pro Tools' agent story can't match" value, fully isolated.
- **Session B:** G-17 video decode (FilmCraft donor; `crates/video` only). Isolated, parallel-safe.
- **Session C:** G-04 correctness bugs — but **only** if A and B stay out of `crates/engine`/`mix` (they do). This stabilizes the base before Wave 2.
- Hold G-08, G-09, G-12, G-19 until G-04 lands and the queue marks them unblocked.

## Priority note (from the review, and I agree)
**Phase 6 — the agent layer (MCP/CLI) — is the product thesis, not a chore after alpha.** Pro Tools has no MCP layer; that is the reason this fork is worth making for an agent-driven / PA-family workflow. Phase 6 should be prioritized **alongside or before** the Phase 7 alpha blockers, not after them.
