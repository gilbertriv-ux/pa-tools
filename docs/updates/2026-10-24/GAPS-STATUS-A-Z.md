# Pa Tools — Complete Gap Status (A–Z)

**Date:** 2026-10-24 · **Branch:** `claude/hardware-m1` · **Base:** `pa-tools` (Pro Tools Ultimate 2026.4.1 target)

Status of all 22 ranked gaps. "This session" = work committed on `claude/hardware-m1`.
Legend: ✅ done · 🟡 partial · ❌ not started · ⚠️ built but unverified (no hardware in cloud container)

---

## HARDWARE (the dimension you asked about — ~12% ready at baseline)

### G-01 — Audio interface · 🟡 partial · alpha blocker
- ✅ Device enumeration, audio-system choice, input/output device, sample rate, buffer size, per-machine prefs (pre-session)
- ✅ ASIO behind opt-in build flag (pre-session)
- ✅ **This session:** latency measurement from cpal callback timestamps; opt-in record-offset compensation (default OFF); `setup.audio_latency` command
- ❌ **Still needed:** true duplex stream on one clock (cpal gives two separate streams); per-channel routing / I/O Setup mapping (bus → physical channels); automatic record-offset; adaptive block size (fixed 512 now); JACK
- ⚠️ Nothing run on a real interface

### G-02 — MIDI devices · 🟡 mostly done · alpha blocker
- ✅ **This session:** port listing, live input + output via `midir` (safe, no unsafe); message parser; MIDI take recorder; wired to transport record into armed MIDI/Instrument tracks; `setup.midi_ports` + `setup.midi_input` commands
- ❌ **Still needed:** live MIDI monitoring / thru (play a hosted synth live from the keyboard); MIDI Beat Clock / MTC output; loop-record one clip per pass (now one long clip); punch range honored; latency-comp applied to MIDI
- ⚠️ Nothing run on a real keyboard (no `/dev/snd/seq` in container)

### G-06 — Record-to-disk / memory · 🟡 partial · alpha blocker (tracking)
- ✅ Recording already streams to disk (`TakeWriter`) — fine
- ❌ **Still needed:** playback/mix still decodes whole files into RAM (`SourceAudio.buffer`) — THIS is the crash-on-long-session bug; safe streamed reads approved, not started

### G-10 — Control surfaces · ❌ nothing
- ❌ No HUI / Mackie Control, no EUCON, no generic MIDI controller mapping (40–70 h)

### G-11 — External sync · ❌ nothing
- ❌ No MTC/LTC chase or generate, no MIDI Beat Clock, no word clock, no video reference (15–25 h; follows G-01/G-02)

---

## FILE FORMATS

### G-03 — Session interchange (AAF/OMF) · 🟡 partial · alpha blocker
- ✅ **This session:** new `soundcraft-interchange` crate — hostile-input-safe CFB (Compound File) reader + AAF identification
- ❌ **Still needed:** parse AAF compositions (tracks/clips/fades), import into a session, export, OMF, `.ptx` (owner decision)
- ⚠️ No real `.aaf` file tested — DEFERRED pending a sample from you

### G-12 — Immersive delivery · ❌ nothing
- ❌ No Dolby Atmos/ADM BWF, MPEG-H, binaural (50–80 h)

### G-17 — Video & post · ❌ nothing
- ❌ No HEVC/DNx decode, no video export, no external timecode (30–50 h)

### G-21 — Compressed export · ❌ nothing
- ❌ No MP3, AAC/M4A, Ogg/Opus bounce (WAV/AIFF/FLAC work) (10–20 h)

---

## FEATURES

### G-07 — Fidelity harness · ❌ nothing
- ❌ No measured comparison against Pro Tools output (null tests, fade/pan-law, plugin response) (20–30 h)

### G-08 — Built-in plugins · ❌ not started
- ❌ 26 processors + 2 instruments vs Pro Tools ~80; generic UIs; no fidelity checks (80–120 h)

### G-15 — Elastic Audio depth · ❌ not started
- ❌ One warp algorithm; no Elastic Pitch, no warp-marker depth (30–45 h)

### G-16 — Session management · ❌ nothing
- ❌ No templates, track presets, workspace browser, batch rename (25–40 h)

### G-20 — MIDI/instrument depth · ❌ nothing
- ❌ No MIDI learn, sampler depth, notation editing (40–60 h)

---

## ECOSYSTEM / PLUGINS

### G-05 — macOS plugin hosting · ❌ not started · (entitlement = alpha blocker for mix)
- ❌ `disable-library-validation` entitlement missing (3rd-party VST3 blocked in signed build); AU editor views missing (15–25 h)

### G-13 — Plugin ecosystem depth · ❌ nothing
- ❌ No sidechain/key input, no ARA, plugins run in-process (crash takes session down) (30–50 h)

### G-22 — Avid cloud services · ❌ owner decision
- ❌ Collaboration, Sketch, Splice, sign-in (out of scope unless you decide otherwise)

---

## STABILITY / PLATFORMS / LOCALIZATION / UI / AI

### G-04 — Correctness bugs · ❌ not touched this session
- ❌ ~25 open bugs in features counted as done (25–40 h)

### G-14 — Platforms & CI · ❌ not started
- ❌ No PR CI running `cargo xtask ci` on 3 OSes; Windows/Linux/Web lightly exercised (20–40 h)

### G-09 — Localization · ❌ nothing
- ❌ English only, no string catalog, no CJK/RTL (70–110 h)

### G-19 — UI feel · ❌ not started
- ❌ Generic plugin windows, shortcut collisions, pro-grade meters/scaling (60–100 h)

### G-18 — Speech-to-text · ❌ nothing
- ❌ Dialogue transcription (needs open local model) (30–50 h)

---

## Summary

| State | Gaps |
|---|---|
| 🟡 Partial (work this session) | G-01, G-02, G-03, G-06 |
| ❌ Not started | G-04, G-05, G-07, G-08, G-09, G-10, G-11, G-12, G-13, G-14, G-15, G-16, G-17, G-18, G-19, G-20, G-21, G-22 |

**Biggest remaining single bug:** G-06 playback RAM decode (crash on long sessions).
**Hardware still open:** G-01 duplex+routing, G-02 monitoring/clock, G-10 surfaces, G-11 sync.
**Everything built this session is unverified on real hardware** — only your Mac can confirm it.
