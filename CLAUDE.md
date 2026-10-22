Read `AGENTS.md`: it is the entry point for agents in this repo, and holds the engineering rules (clean-room, never crash, layering, everything is a command) and Gil's hard gates.

The short version, for every session:

- **Pa Tools** is a fork of SoundCraft (see `UPSTREAM.md`). Crate names, the `.scraft` format and the `SOUNDCRAFT_*` environment variables stay; user-visible strings come from `crates/brand`; run `./rebrand.sh` after merging upstream.
- **Never overwrite, delete, move or rename anything on Gil's drives.** Copy to a new path; a redo is a new version in a new folder. Write only to `4 TB Q`. Announce every folder and file you create, with its full path.
- **Clean-room.** No Avid code or assets, no reverse-engineering of Pro Tools, no GPL/AGPL/LGPL code (FilmCraft is an allowed donor). Third-party product names never appear in the UI.
- **Never crash.** No `unwrap`/`expect`/`panic!` in non-test code; `unsafe` only in the three plugin-host crates; errors are values.
- **Before every commit:** `cargo xtask ci`. Every new bundled asset gets a row in `ATTRIBUTION.md`.
- Keep to the main work; hand correspondence to another agent.
