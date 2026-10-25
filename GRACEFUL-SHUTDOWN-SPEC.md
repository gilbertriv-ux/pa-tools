# Graceful Shutdown — Implementation Spec for Codex

**Audience:** Codex (owner of `crates/playback` and `crates/mix`).
**Author:** Claude (cloud). I did not edit any audio-lane code — this is a spec only.
**Branch base:** `claude/alpha-integration` @ `7b3a4b4`.
**Symptom:** macOS shows "Pa Tools quit unexpectedly" every time the app closes.
Affects both the Claude and Codex builds.

All file/line references are against `7b3a4b4`.

---

## 0. Root cause (what I found)

On quit, `apps/soundcraft/src/main.rs::on_exit` (lines 87–97) drops the `Player`
(`self.0.player = None`) and returns. The process then begins tearing down. The
crash comes from **background threads that are still alive at that moment**, plus
one `Player` teardown path that has no join at all:

1. **The silent-clock thread is detached and never joined.**
   `Player::silent_clock_with` (`crates/playback/src/lib.rs:651`) spawns the
   `"soundcraft-clock"` thread but **does not store its `JoinHandle`** — `let spawn
   = …spawn(…); if let Err(e) = spawn { … }` only inspects the error and drops the
   handle, detaching the thread. Its only exit signal is a poll of
   `Arc::strong_count(&state.shared) <= 1` every 10 ms (line 664). When `Player`
   drops, `on_exit` returns immediately; the detached thread is very likely still
   inside its `sleep(10 ms)` / `state.fill(...)` and touches `state` (plugins,
   rings, session) while the main thread tears the process down → abort/segfault →
   the crash dialog. **This is the primary suspect.** It fires whenever the app ran
   without an audio device, or fell back to the silent clock.

2. **`Player` has no `Drop` impl** (confirmed: no `impl Drop for Player` anywhere
   in `crates/playback`). Teardown relies on struct field-drop order:
   `_stream` (`:496`) then `tx: Sender<Cmd>` (`:493`). For the **device path** the
   CPAL callback owns the `AudioState` and drains the `Cmd` queue with a
   non-blocking `try_recv` (`:225`), so dropping `_stream` is what actually stops
   it — but nothing guarantees `_stream` is dropped *before* the rest of `Player`,
   and nothing joins or coordinates the clock thread from path (1).

3. The **disk-streaming reader** (`StreamWorker`, `crates/mix/src/lib.rs:1921`) is
   already correct — `stop: Arc<AtomicBool>` + stored `JoinHandle`, Drop sets stop
   and joins (`:1935`). It only crashes at exit if the struct that **owns** it is
   leaked (held by a static or a surviving `Arc`) instead of being dropped during
   teardown. Verify ownership; see §5.

4. **MIDI** input holds a `midir::MidiInputConnection` (`crates/playback/src/
   midi_io.rs:299–300`), whose OS callback thread closes on `Drop`. Same caveat as
   (3): it must actually be dropped on exit, not leaked.

The fix is to give `Player` a deterministic, panic-free, non-blocking teardown
that (a) drops the CPAL stream first, (b) signals the clock thread and joins it,
and to make `on_exit` call it in the right order.

---

## 1. `Drop` traits to implement or harden

### 1a. `Player` — **implement teardown** (`crates/playback/src/lib.rs:487`)
Add an explicit `fn shutdown(&mut self)` AND an `impl Drop for Player` that calls
it. `shutdown` must be **idempotent** (safe to call from both `on_exit` and
`Drop`) — achieved by `Option::take()` on every handle so the second call is a
no-op. It must not `unwrap`/`expect`/`panic`, and must not block indefinitely
(see §3). Required fields to add to `Player`:
- `clock: Option<std::thread::JoinHandle<()>>` — the clock thread's handle
  (currently detached; store it).
- `stop: Arc<AtomicBool>` — explicit stop flag for the clock thread (replaces the
  implicit `strong_count` poll as the primary exit signal; keep the strong_count
  check only as a backstop).

### 1b. `StreamWorker` — **no change needed** (`crates/mix/src/lib.rs:1935`)
Already the gold-standard pattern: atomic `stop`, stored handle, `worker_loop`
parks with `park_timeout(WORKER_POLL)` when idle (`:1985`) so `join()` returns
within one poll interval. Use this as the template for `Player`'s clock thread.
Only action: confirm its owner is dropped at exit (§5).

### 1c. `Recorder` — **harden / verify** (`crates/playback/src/record.rs:372`)
`Drop` calls `finish()`, which sets `armed=false` then joins the writer thread.
The writer (`write_takes`) polls `armed` every `sleep(10 ms)` and breaks, so the
join is bounded. Verify:
- `finish()` is **idempotent** — it is today (`self.writer.take()…
  unwrap_or_default()`), so a `Drop` after an explicit `finish()` double-call is a
  no-op. Keep it that way.
- `Drop` performs a final disk flush; that is acceptable but is the one place a
  slow disk could stall exit. If that is a concern, bound it (see §3). No panic
  risk today (mutex guarded with `PoisonError::into_inner`). Leave as-is unless §5
  shows the recorder is live at exit.

### 1d. `Autosaver` — **no change needed** (`crates/engine/src/autosave.rs:86`)
Already drops its `jobs` sender (ending the writer's `recv`) then joins. This is
the model `on_exit`'s `close_autosave()` already relies on.

---

## 2. Explicitly dropping the CPAL stream before joining threads

Inside `Player::shutdown`, in this exact order:

1. **Drop the stream first:** `let _ = self._stream.take();`
   Taking the `Option` drops the `cpal::Stream`, which stops the output callback
   and releases the `AudioState` the callback owned (plugins, rings). Do this
   **before** touching the clock thread or `side`, so the audio callback is no
   longer running when the rest is torn down. cpal handles the OS stream stop in
   the stream's own `Drop`; do not call `pause()` first (not required, and a
   `StreamTrait::pause` error would need handling) — dropping is sufficient.
2. For the **silent path** there is no stream (`_stream == None`), so `take()` is a
   harmless no-op and step 3 does the real work.

Field-drop order still matters as a backstop: declare `_stream` **above** `tx`,
`side`, and the new `clock`/`stop` fields in the struct, so even a plain drop of
`Player` (without `shutdown`) drops the stream first.

---

## 3. Breaking the loops cleanly without blocking the main thread

### Clock thread (`silent_clock_with`, `:651`)
- Change the loop's exit to check the explicit flag **first**:
  break when `stop.load(Acquire)` is true; keep `Arc::strong_count(&shared) <= 1`
  as a secondary guard.
- Replace the unconditional `sleep(10 ms)` at the top of the loop so a stop is
  observed promptly. Two acceptable options:
  - **Preferred:** mirror `StreamWorker` — `park_timeout(POLL)` at the idle point
    and `self.clock`'s thread is `unpark()`ed right after `stop.store(true)` in
    `shutdown`, so `join()` returns almost immediately.
  - **Minimal:** keep the 10 ms sleep; `join()` then returns within ≤10 ms, which
    is acceptable for exit. Either is fine; the first avoids any perceptible hang.
- In `shutdown`: `self.stop.store(true, Release);` then
  `if let Some(j) = self.clock.take() { let _ = j.join(); }`. `let _ = join()`
  keeps `Drop` panic-free even if the thread already ended.

### Device path `Cmd` loop (`AudioState::drain`, `:225`)
- No change required: it is a **non-blocking `try_recv`** inside the CPAL callback.
  It stops when the stream is dropped (step 2.1). Do **not** add a blocking
  `recv()` on the shutdown path — that is the one thing that would hang exit.

### General rule for every join on the exit path
- Only ever join a thread whose stop signal you have **already set** (atomic flag
  set, or sender dropped) earlier in the same function. Set-then-join, never
  join-then-hope. No join on the exit path may depend on disk or network I/O that
  is unbounded; the stream, clock, autosave, and stream-preread joins above are
  all bounded by a poll interval. If `Recorder`'s final flush (§1c) is judged
  unbounded, cap it (e.g., stop writing new blocks after a fixed budget and close
  the files) rather than letting `join()` wait on the disk.

---

## 4. `on_exit` sequence (`apps/soundcraft/src/main.rs:87`)

This file is in the app/UI lane (not `mix`/`playback`); I can wire it once
`Player::shutdown` exists, or Codex can — flag your preference. The desired order:

1. **Quiet the transport.** If playing, stop it (`self.0.player` → `Stop`) so the
   callback is producing silence before teardown. Optional but tidy.
2. **Finish recording** if a `Recorder` is active, so takes are flushed through the
   normal path before the player goes away.
3. **`close_autosave()`** — unchanged; `Autosaver::Drop` already joins its writer.
4. **Tear down the player explicitly and first among audio objects:**
   `if let Some(p) = self.0.player.take() { p into shutdown + drop }`. With
   `Player::shutdown`/`Drop` from §1a this drops the CPAL stream, sets the clock
   stop flag, and joins the clock thread deterministically. Any `StreamWorker` and
   `MidiInputConnection` owned by the session/mixer drop here too (§5), joining /
   closing their threads.
5. **`shutdown_plugin_hosts()`** — **after** the player is fully torn down, so no
   callback or plugin instance is live when hosting shuts down. (Today it runs
   after `player = None`; keep that relative order.)
6. **`save_prefs(...)`** — order-independent (no audio state); can stay where it is.

The invariant: **stream → clock/worker threads joined → plugin hosts → done**, with
every join preceded by its stop signal, and nothing on the path blocking on I/O.

---

## 5. Ownership checks to confirm before implementing

The `Drop`-based joins only run if the owning value is actually dropped at exit
(not parked in a `static`, a leaked `Arc`, or `std::mem::forget`):

- [ ] **`StreamWorker`** — trace its owner (the mixer/session streaming state) and
      confirm it is reachable from `Player`/`Engine` and dropped when the player
      drops. If an `Arc` cycle keeps it alive, break the cycle or drop it
      explicitly in `shutdown`.
- [ ] **`MidiInputConnection`** (`midi_io.rs:299`) — confirm the owning `MidiInput`
      is dropped on exit so midir closes the OS callback thread; if the UI holds it,
      drop it in `on_exit` before/with the player.
- [ ] **CPAL `AudioState`** — owned by the stream callback on the device path;
      dropping `_stream` drops it. Confirm no second `Arc` to its plugins survives.
- [ ] Confirm `Player` is the **only** owner of the clock `JoinHandle` once stored,
      so the join can't be attempted twice.

---

## 6. Definition of done (verification, Codex side)

- Launch and quit **with** an audio device → no crash dialog.
- Launch and quit **with no device / silent clock** → no crash dialog (this is the
  path the detached clock thread breaks today).
- Quit **while playing**, **while a stream (disk-backed) clip plays**, and **while
  recording** → no crash, takes flushed.
- Quit **with a MIDI input connected** → no crash.
- `cargo xtask ci` green; no `unwrap`/`expect`/`panic` added to non-test code; no
  new `unsafe`; audio-thread realtime rules intact (no new alloc/lock on the
  callback — all new work is on the shutdown path, off the callback).
- Exit is prompt (no multi-second hang): every join on the path is bounded by a
  poll interval, not by I/O.

---

## 7. Scope / coordination

- All code changes land in `crates/playback` (`Player`, clock thread), optionally
  `crates/playback/src/record.rs` and `crates/mix` ownership, and
  `apps/soundcraft/src/main.rs` (`on_exit`).
- `StreamWorker` and `Autosaver` are already correct; prefer not to touch them
  beyond the ownership check.
- Claude will not edit `mix`/`playback` in parallel while Codex holds this. The
  `on_exit` wiring (app lane) can be done by either side — say which you prefer.
