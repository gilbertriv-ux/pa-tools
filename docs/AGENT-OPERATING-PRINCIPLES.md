# Agent Operating Principles — Pa Tools

These are the durable, cross-session rules for any agent (Claude, Codex, or other)
working on Pa Tools. They sit on top of `AGENTS.md` (engineering rules) and
`CLAUDE.md` (the short version). Read all three at session start.

## Hard gates (never violate)

1. **No deletion/overwrite on Gil's machines.** Never delete, move, rename or
   overwrite anything under `/Volumes`, `/Users`, `~`, or `/Applications`. To
   relocate, copy to a new path and leave the original. A redo is a NEW version in
   a NEW folder (V1, V2, …). Writes to Gil's machines go only to `4 TB Q`.
   Enforced by `.claude/hooks/block-drive-deletes.sh`.
2. **Cloud scratch is fine to delete.** Build output and caches the agent itself
   created inside the cloud container (`/tmp`, `target/`) may be removed to free
   disk — but always name the exact path you removed.
3. **No `unsafe`.** `unsafe_code = "forbid"` workspace-wide. No `unsafe`, no
   `memmap`/`mmap`. `unsafe` lives only in the three plugin-host crates
   (`clap-host`, `vst3-host`, `au-host`). This is why we use streamed reads, not
   memory mapping.
4. **Never crash.** No `unwrap`/`expect`/`panic!`/`unreachable!`/`todo!` in
   non-test code. Errors are `Result` values. Hostile numeric input uses
   checked/saturating math and bounded allocation.
5. **Clean-room.** No Avid code or assets, no reverse-engineering of Pro Tools, no
   GPL/AGPL/LGPL code (FilmCraft is an allowed donor). No third-party product
   names in the UI.
6. **No hallucinated facts.** Never invent commit SHAs, file paths, test results,
   or RAM/latency numbers. Report only what a command actually produced. Separate
   "verified here" from "needs Gil's hardware to confirm."

## Always Be Building (ABB) mandate

- Do not sit idle waiting for feedback when there is authorized work queued.
- The instant one workstream lands, verify it, commit it, and launch the next —
  no idle gap.
- **Concurrency is bounded by physical reality**, not enthusiasm:
  - Two agents must not edit the same file concurrently (corruption / unmergeable
    conflicts). Only fan out across DISJOINT file sets.
  - This container has one ~30 GB disk; a single full `cargo` build nearly fills
    it. Do NOT run two full concurrent builds — they crash on "no space".
  - Therefore ABB here = a tight SEQUENTIAL pipeline with zero idle, plus true
    parallelism only for non-compiling work (docs, planning, read-only analysis)
    or genuinely disjoint small builds that fit the disk.

## Audio-thread rules (realtime safety)

The audio callback must NEVER block, allocate, or panic.
- Worker↔callback communication uses a lock-free SPSC ring (`rtrb`). No `Mutex`,
  `RwLock`, or `std::sync::mpsc` on any path the callback touches.
- Ring underrun → output silence for the missing frames (log at most once), never
  wait.
- Disk reads and peak-building happen on background threads, never in the callback
  or blocking playback start.

## Workflow discipline

- Work on the `claude/hardware-m1` branch. No PRs or merges to `main`/`pa-tools`
  unless Gil asks.
- Agents build/test but do NOT commit; the coordinator verifies
  (`cargo fmt --check`, `clippy -D warnings`, tests, `cargo xtask layers`, and a
  grep for `unsafe`/`mmap`) and commits per topic.
- Commit messages state honestly what is verified vs unverified.
- Gil pulls to his Mac / `4 TB Q` manually; the cloud never writes to his drives.
