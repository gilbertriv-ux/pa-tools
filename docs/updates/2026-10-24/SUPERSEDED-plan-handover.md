# PA Tools Alpha Blockers — M1 Implementation Handover

**From:** Claude Code (cloud session)  
**To:** Codex (desktop session)  
**Date:** 2026-10-24 07:04 PST  
**Deadline:** Monday 2026-10-27 4pm PST (65 hours remaining)  
**Budget:** 20% cloud credits remaining ($54 of $250)

---

## Executive Summary

Three alpha blockers block PA Tools from shipping. M1 (foundation) for each is fully designed and ready to code. All three M1s can complete in **7–10 wall-clock hours with a 10-agent swarm** (all blockers in parallel).

**Goal:** Complete all M1s by Sunday night. Start M2s Monday morning if time permits.

---

## Three Alpha Blockers (M1 Plans Complete)

### **G-01: Audio Interface Duplex Stream**
- **Status:** Device enumeration done (2026-10-12); duplex/latency missing
- **M1 Scope:** DuplexStream trait, ASIO duplex impl, Player integration, latency APIs
- **Commits:** 7 commits
- **Files:** Create `duplex.rs`, `asio_duplex.rs`, `latency.rs`; modify `lib.rs`, `record.rs`, `devices.rs`
- **LOC:** ~1,560
- **Est. Hours:** 15–20 (sequential); **2–3 hours (10-agent swarm)**
- **Branch:** `pa-tools/feature/g-01-m1`

### **G-02: MIDI Device I/O**
- **Status:** MIDI data layer complete; zero hardware I/O
- **M1 Scope:** MidiSystem trait, platform bindings (macOS CoreMIDI, Windows WinMM, Linux ALSA)
- **Commits:** 7 commits
- **Files:** Create `midi_devices.rs` + platform modules; modify `playback/lib.rs`, `record.rs`
- **LOC:** ~4,500–6,000
- **Est. Hours:** 25–35 (sequential); **3–5 hours (10-agent swarm)**
- **Branch:** `pa-tools/feature/g-02-m1`

### **G-03: Session Interchange (AAF/OMF)**
- **Status:** Not started; no AAF/OMF support
- **M1 Scope:** CFB binary parser, AAF object model, validation layer (new `pa-interchange` crate)
- **Commits:** 7 commits
- **Files:** New crate `crates/pa-interchange/` with CFB, AAF, validation modules
- **LOC:** ~3,860 (code + tests + docs)
- **Est. Hours:** 32–45 (sequential); **4–6 hours (10-agent swarm)**
- **Branch:** `pa-tools/feature/g-03-m1`

---

## Swarm Strategy: 10 Agents Parallel

Run agents in this configuration to complete all M1s by Saturday night:

```
[G-01 Swarm]                    [G-02 Swarm]                    [G-03 Swarm]
├─ Agent 1: duplex.rs           ├─ Agent 4: midi_devices.rs      ├─ Agent 7: pa-interchange scaffold
├─ Agent 2: asio_duplex impl     ├─ Agent 5: platform bindings    ├─ Agent 8: CFB reader
├─ Agent 3: Player integration   ├─ Agent 6: tests & polish       ├─ Agent 9: AAF parser
                                                                   └─ Agent 10: AST + validator
```

**Parallel wall-clock:** 4–6 hours for all three M1s to complete (vs. 72–100 sequential).

---

## G-01 M1: Audio Interface Duplex Stream

### Commits (7 total)

1. **Core DuplexStream trait and latency types** (280 LOC)
   - Create: `crates/playback/src/duplex.rs`
   - Define `DuplexStream` trait, `StreamLatency`, error types
   - No platform code yet, just trait definition

2. **ASIO duplex stream implementation** (450 LOC, Windows-specific)
   - Create: `crates/playback/src/asio_duplex.rs`
   - Wrap cpal's ASIO for single synchronized clock
   - Query driver latency via ASIO callbacks
   - Test: ASIO stream open/close, latency measurement

3. **Duplex fallback for non-ASIO platforms** (200 LOC)
   - Update: `duplex.rs`
   - Fallback impl for WASAPI/CoreAudio/ALSA (two synced streams)
   - Platform detection & factory function

4. **Player integration and latency APIs** (130 LOC)
   - Modify: `crates/playback/src/lib.rs`
   - Refactor Player to use DuplexStream instead of dual streams
   - Add `input_latency()`, `output_latency()` methods
   - Wire latency context to callbacks

5. **Latency callback and measurement** (80 LOC)
   - Modify: `crates/playback/src/record.rs`
   - Enable timestamp reporting from audio callbacks
   - Round-trip latency measurement infrastructure

6. **Device latency queries** (100 LOC)
   - Modify: `crates/playback/src/devices.rs`
   - Query drivers (ASIO) or estimate from buffer size
   - Latency helper functions

7. **Integration tests** (300 LOC)
   - Create: `crates/playback/tests/duplex_stream.rs`, `latency_measurement.rs`
   - Duplex sync verification, latency consistency, input/output alignment

### Files Summary

**Create:**
- `crates/playback/src/duplex.rs` (Core trait + types, 280 LOC)
- `crates/playback/src/asio_duplex.rs` (ASIO impl, 450 LOC)
- `crates/playback/tests/duplex_stream.rs` (Tests, 300 LOC)
- `crates/playback/tests/latency_measurement.rs` (Tests, 150 LOC)

**Modify:**
- `crates/playback/src/lib.rs` (+400 LOC, Player refactor)
- `crates/playback/src/devices.rs` (+100 LOC, latency queries)
- `crates/playback/src/record.rs` (+30 LOC, latency integration)

### Success Criteria

✅ DuplexStream trait compiles on all platforms  
✅ ASIO duplex stream opens and closes without deadlock  
✅ Latency values match device specs (within 10%)  
✅ `cargo xtask ci` passes on all commits  
✅ No `unwrap`, `expect`, `panic!` in non-test code  
✅ Tests verify input/output sync within ±1 sample  

### Key Commands

```bash
cd /home/user/pa-tools
git checkout -b pa-tools/feature/g-01-m1
# ... code commits 1–7 ...
cargo xtask ci  # Before each commit
git push -u origin pa-tools/feature/g-01-m1
```

---

## G-02 M1: MIDI Device I/O

### Commits (7 total)

1. **Core MIDI types & traits** (400–500 LOC)
   - Create: `crates/playback/src/midi_devices.rs`
   - Define `MidiError`, `MidiMessage`, `MidiPort`, `MidiSystem` trait
   - Platform-agnostic interface

2. **macOS CoreMIDI binding** (500–700 LOC)
   - Create: `crates/playback/src/midi_devices/macos.rs`
   - Unsafe bindings to CoreMIDI
   - Device enumeration, hot-plugging support

3. **Windows WinMM binding** (550–750 LOC)
   - Create: `crates/playback/src/midi_devices/windows.rs`
   - Unsafe bindings to MME API
   - Device enumeration, change notifications

4. **Linux ALSA binding** (550–750 LOC)
   - Create: `crates/playback/src/midi_devices/linux.rs`
   - Unsafe bindings to ALSA sequencer
   - Port enumeration & subscriptions

5. **Trait implementations & API** (300–400 LOC)
   - Update: `crates/playback/src/midi_devices.rs`
   - `impl MidiSystem` for all platforms
   - Public functions: `enumerate_midi_inputs()`, `enumerate_midi_outputs()`
   - Platform factory function

6. **Playback/record integration** (250–350 LOC)
   - Modify: `crates/playback/src/lib.rs`, `record.rs`
   - Extend Player with MIDI system
   - Lock-free callback ring for MIDI messages

7. **Tests & documentation** (400–500 LOC)
   - Create: `crates/playback/tests/midi_devices.rs`
   - 40 test cases (32 unit + 8 integration)
   - Module docs, architecture overview

### Files Summary

**Create:**
- `crates/playback/src/midi_devices.rs` (Main module)
- `crates/playback/src/midi_devices/macos.rs` (CoreMIDI)
- `crates/playback/src/midi_devices/windows.rs` (WinMM)
- `crates/playback/src/midi_devices/linux.rs` (ALSA)
- `crates/playback/tests/midi_devices.rs` (Tests)

**Modify:**
- `crates/playback/src/lib.rs` (+50 LOC)
- `crates/playback/src/record.rs` (+200–300 LOC)

### Success Criteria

✅ Device enumeration returns ≥1 input/output on test machine  
✅ No unsafe code outside platform modules  
✅ All errors return `Result<T, MidiError>`  
✅ `cargo xtask ci` passes  
✅ Tests pass on macOS, Windows, Linux (or documented unsupported)  
✅ `enumerate_midi_inputs()` callable from UI layer  

### Key Commands

```bash
cd /home/user/pa-tools
git checkout -b pa-tools/feature/g-02-m1
# ... code commits 1–7 ...
cargo xtask ci
git push -u origin pa-tools/feature/g-02-m1
```

---

## G-03 M1: Session Interchange (AAF Parser)

### Commits (7 total)

1. **Create pa-interchange crate scaffold** (155 LOC)
   - Create: `crates/pa-interchange/Cargo.toml`, `src/lib.rs`
   - Create: `crates/pa-interchange/src/cfb/header.rs`
   - Define CFB header structures and constants
   - Modify: Root `Cargo.toml` (add workspace member)

2. **CFB container binary parser** (465 LOC)
   - Create: `crates/pa-interchange/src/cfb/reader.rs`, `directory.rs`
   - Create: `crates/pa-interchange/src/error.rs`
   - Parse sector allocation table, directory tree

3. **AAF binary format parser** (510 LOC)
   - Create: `crates/pa-interchange/src/aaf/object.rs`, `types.rs`, `metadata.rs`
   - Parse AAF objects from CFB container
   - Handle all primitive types, big-endian decoding

4. **AAF object model (AST)** (515 LOC)
   - Create: `crates/pa-interchange/src/aaf/ast.rs`, `builder.rs`
   - Build Abstract Syntax Tree from parsed objects
   - Composition, track, clip, effect structures

5. **AAF validation layer** (415 LOC)
   - Create: `crates/pa-interchange/src/aaf/validator.rs`, `diagnostics.rs`
   - Structural validity checks, type correctness, semantic constraints
   - Diagnostic reporting

6. **Integration tests** (250 LOC)
   - Create: `tests/integration_cfb_aaf.rs`
   - Create: `tests/fixtures/sample.aaf` (test fixture)
   - End-to-end CFB→AAF→AST→validation pipeline

7. **Documentation & examples** (390 LOC)
   - Create: `crates/pa-interchange/README.md`, `examples/parse_aaf.rs`
   - Create: `docs/G-03-M1-design.md` (implementation notes)
   - Crate-level docs, public API, examples

### Files Summary

**Create:**
- New crate: `crates/pa-interchange/` (entire directory)
  - `Cargo.toml`
  - `src/lib.rs`, `src/cfb/mod.rs`, `src/cfb/header.rs`, `src/cfb/reader.rs`, `src/cfb/directory.rs`
  - `src/error.rs`, `src/aaf/mod.rs`, `src/aaf/object.rs`, `src/aaf/types.rs`, `src/aaf/metadata.rs`
  - `src/aaf/ast.rs`, `src/aaf/builder.rs`, `src/aaf/validator.rs`, `src/aaf/diagnostics.rs`
  - `tests/`, `examples/`, `README.md`

**Modify:**
- `/home/user/pa-tools/Cargo.toml` (add pa-interchange as workspace member)

### Success Criteria

✅ `cargo xtask ci` passes (fmt, clippy, layers, tests)  
✅ Parse a real AAF file without panicking  
✅ AST represents sessions, tracks, clips, effects  
✅ Validator catches missing UIDs and type errors  
✅ `cargo doc -p pa-interchange` builds with no warnings  
✅ Example code compiles and demonstrates parsing  

### Key Commands

```bash
cd /home/user/pa-tools
git checkout -b pa-tools/feature/g-03-m1
# ... code commits 1–7 ...
cargo xtask ci
git push -u origin pa-tools/feature/g-03-m1
```

---

## Parallel Development Workflow

### Branch Structure

```
pa-tools/ (main)
├─ feature/g-01-m1  (Audio interface duplex)
├─ feature/g-02-m1  (MIDI device I/O)
└─ feature/g-03-m1  (AAF parser)
```

### Agent Assignment (10-Agent Swarm)

**G-01 Team (Agents 1–3):** 2–3 hours
- Agent 1: Duplex trait + ASIO impl (commits 1–2)
- Agent 2: Player integration + latency (commits 3–5)
- Agent 3: Tests + polish (commits 6–7)

**G-02 Team (Agents 4–6):** 3–5 hours
- Agent 4: Core types + macOS CoreMIDI (commits 1–2)
- Agent 5: Windows + Linux bindings (commits 3–4)
- Agent 6: Trait impl + integration + tests (commits 5–7)

**G-03 Team (Agents 7–10):** 4–6 hours
- Agent 7: Crate scaffold + CFB reader (commits 1–2)
- Agent 8: AAF parser + object model (commits 3–4)
- Agent 9: Validator + integration tests (commits 5–6)
- Agent 10: Docs + examples + done-gate (commit 7)

### Merge Strategy

1. **Saturday night:** All M1 commits merged to `feature/g-XX-m1` branches
2. **Sunday morning:** Rebase onto main, run `cargo xtask ci` on integrated tree
3. **Sunday afternoon:** Merge to main (all three M1s together)
4. **Monday morning:** Start M2 work if time permits

---

## Success Gate: All M1s Done

- [ ] G-01 M1: 7 commits, all passing `cargo xtask ci`
- [ ] G-02 M1: 7 commits, all passing `cargo xtask ci`
- [ ] G-03 M1: 7 commits, all passing `cargo xtask ci`
- [ ] All three branches rebased onto main
- [ ] `cargo test --workspace` passes
- [ ] `cargo xtask layers` passes (no layer violations)
- [ ] `cargo xtask parity` unchanged
- [ ] No `unwrap`, `expect`, `panic!` in non-test code across all changes
- [ ] Ready for M2: Recording works, MIDI enumeration works, AAF parse works

---

## Environment & Tooling

**Repo:** `/home/user/pa-tools` (already cloned)

**Required commands (run before each commit):**
```bash
cargo fmt --all
cargo clippy -p soundcraft-playback -D warnings
cargo xtask ci
cargo test -p soundcraft-playback  # or -p pa-interchange for G-03
```

**Git workflow:**
```bash
git checkout -b pa-tools/feature/g-XX-m1
# ... code commits ...
cargo xtask ci  # Before push
git push -u origin pa-tools/feature/g-XX-m1
```

**Rules from AGENTS.md:**
- No `unwrap`, `expect`, `panic!` in non-test code
- Errors are `Result<T, E>`
- One topic per commit
- Reference blocker ID in commit message: `feat(g-01): ...`, `fix(g-02): ...`
- Safe wrapper APIs around platform-specific `unsafe` code

---

## Deliverables

By Monday 4pm PST, deliver:
- ✅ All three M1s merged to main
- ✅ `cargo xtask ci` passing on integrated tree
- ✅ Recording functional (G-01)
- ✅ MIDI device enumeration working (G-02)
- ✅ AAF files parseable (G-03 M1 only; importer comes in M2)

---

## Next Steps (M2 Work)

If time permits Monday:
- **G-01 M2:** Latency compensation + record-offset (6–8 h)
- **G-02 M2:** Real-time input integration (7–9 h)
- **G-03 M2:** AAF → Session importer (20–26 h)

But M1s are the critical path. Get those done first.

---

## Questions for Codex

1. Can you access pa-tools repo at `/home/user/pa-tools`?
2. Do you have ASIO SDK headers for Windows ASIO impl?
3. Do you have CoreMIDI + WinMM + ALSA dev headers available?
4. Can you push to GitHub if merge conflicts arise?

---

**Status:** Ready to hand off. All design & planning complete. Ready to code.
