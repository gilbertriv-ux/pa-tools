# Upstream: SoundCraft

Pa Tools is a fork of **SoundCraft** (https://github.com/storytold/soundcraft) by the ArtCraft team
and the SoundCraft contributors, MIT OR Apache-2.0. This file records where the fork started, how to
bring upstream changes in, and what Pa Tools deliberately leaves alone so that merging stays cheap.

## Fork point

| | |
|---|---|
| Upstream repository | `https://github.com/storytold/soundcraft` |
| Commit | `9b8283b90877813a929f5e28b2dc92e5774da933` ("security: enforce HTTPS on curl downloads (SonarCloud S6506) (#860)") |
| Commit date | 2026-10-21 |
| Upstream version at that commit | 0.11.0 (tag `v0.11.0` is an ancestor) |
| Pa Tools branch | `pa-tools` |

## Last merged

Pa Tools has its own version; this section records only which upstream version was merged last.

| | |
|---|---|
| Upstream version | SoundCraft 0.12.1 |
| Commit | `e85a480` ("Release 0.12.1"), merged by SHA, not `upstream/main` |
| Merged on | 2026-10-23 |
| Left out | the README icon commits (`24543e1`, `b1466e4`) and upstream's `52471b3` dependency bump |

## Merging upstream (weekly)

Upstream moves fast, so merge small and often. `origin` still points at upstream in the first
checkout; once Pa Tools has its own repository, run `git remote rename origin upstream` and add the new
`origin`.

```sh
git fetch upstream                                  # or `origin` before the rename
git switch pa-tools && git switch -c merge-upstream-YYYY-MM-DD
git merge upstream/main                             # or a reviewed upstream commit (see "Last merged")
./rebrand.sh                                        # re-applies the packaging/doc brand; prints leftovers
cargo xtask ci                                      # fmt, clippy, tests, assets, layers, wasm
git log --oneline pa-tools..upstream/main           # what came in, for the changelog
```

Resolve conflicts like this:

- **Rust sources.** Take upstream's change and keep Pa Tools' routing of user-visible strings through
  the brand crate (`crates/brand`). After the merge, a grep for brand strings in quotes finds any new
  upstream "SoundCraft" or "ArtCraft" text that still needs routing:
  `grep -rn --include=*.rs -E '"[^"]*(SoundCraft|ArtCraft)[^"]*"' crates apps`.
- **`packaging/`, `docs/`, `.github/`, `ROADMAP.md`, `sonar-project.properties`.** Take upstream's
  change, then run `./rebrand.sh`; it rewrites the brand strings and reports anything it cannot decide.
  Upstream renames of files we renamed (`packaging/linux/com.imagescouture.patools.*`,
  `packaging/windows/patools.wxs`) are followed by git's rename detection; if a stray
  `ai.storyteller.soundcraft.*` or `soundcraft.wxs` appears, move its content into our file and delete it.
- **`README.md`, `AGENTS.md`, `CLAUDE.md`, `ATTRIBUTION.md`, `NOTICE`.** Keep ours. Copy over only upstream
  facts that changed (new commands, new assets, new dependencies) and add any new ATTRIBUTION rows,
  because `cargo xtask assets` fails for an asset file without a row.
- **`docs/brand/`.** Must stay absent. If upstream adds or changes ArtCraft marks, do not take them
  (see "Licence requirement" below); `./rebrand.sh` warns when any `artcraft-*` file is present.
- **`Cargo.lock`.** Take upstream's, then `cargo build --locked` once to confirm it still resolves.
- **Version.** Pa Tools has its own version number (`cargo xtask version`). Resolve every `version =`
  hunk in `Cargo.toml` and `Cargo.lock` to ours; never take upstream's bump.

## Intentionally unchanged

These stay as upstream has them. Renaming them has no benefit for a user and costs a conflict on
every merge (about 1,800 crate-name references in 255 files).

| What | Why it stays |
|---|---|
| Crate and package names: `soundcraft-time`, `-audio-io`, `-midi`, `-video`, `-dsp`, `-clap-host`, `-vst3-host`, `-au-host`, `-model`, `-mix`, `-engine`, `-playback`, `-automation`, `-ui-egui`, and the apps `soundcraft`, `soundcraft-cli`, `soundcraft-web`; Rust module paths (`soundcraft_engine::...`) | Invisible to users. The binaries are renamed with `[[bin]]` entries (`pa-tools`, `pa-tools-cli`); `cargo build -p soundcraft` still selects the app. |
| `xtask` names and checks (`LAYERS`, `UI_CRATES`, `EXEMPT`, `WASM`) | They key off the crate names above. |
| Session file format: `.scraft` (JSON), `.scraftz` (web bundle), `.scgrp` (clip groups), `format_version` 1 | Pa Tools opens upstream sessions and upstream opens Pa Tools sessions. Only the UTI (`com.imagescouture.patools.session`) and MIME type (`application/x-patools-session`) are new. |
| Environment variables `SOUNDCRAFT_*` (`NO_PREFS`, `BUILD_SHA`, `BUILD_DATE`, `CONTROL_PORT`, `CONFIG_DIR`, `REQUIRE_WINRES`, `VERSION`, `MAINTAINER`, `VST3_SMOKE`, `FADE_SHOTS`, `NO_SYSTEM_FONTS`, `MIX_PROOF_DIR`) | Upstream scripts and CI keep working. New `PATOOLS_*` spellings may be added next to them; the old names are never removed. |
| The `craft-ui` and `craft-docpath` git dependencies (`github.com/storytold/craft-libs`, MIT OR Apache-2.0) | Small and pinned by revision. Vendoring them as path dependencies, so the build no longer needs the storytold organisation, is a planned change. The package name `craft-ui` must stay, because `xtask` checks for it. |
| `LICENSE-MIT`, `LICENSE-APACHE` | Unchanged, as the licences require. |
| The upstream notice in `NOTICE` | Kept; Pa Tools adds its own line above it. |

## What Pa Tools changes

- **Brand.** Name Pa Tools, bundle id `com.imagescouture.patools`, publisher GR8 ID YUZ.AI (Images
  Couture), homepage `https://gr8idyuz.ai` (a placeholder until confirmed). User-visible strings come
  from `crates/brand`; packaging and docs are rebranded by `rebrand.sh`.
- **Binaries.** `pa-tools` (desktop app) and `pa-tools-cli` (headless CLI and MCP server).
- **Icon.** A new original icon (`assets/app-icon/icon.svg`) and a regenerated icon set.
- **macOS signing.** `com.apple.security.cs.disable-library-validation` in `entitlements.plist` so signed
  builds load third-party VST3, CLAP and Audio Unit bundles (upstream gap G-05), for the app and the CLI.
- **Windows installer.** `packaging/windows/patools.wxs` has its own `UpgradeCode` and component GUIDs,
  so a Pa Tools install never upgrades or removes a SoundCraft install.
- **Agent instructions.** `AGENTS.md` and `CLAUDE.md` are rewritten; the upstream shared-standards
  repository and the local `plan/` folder do not exist here.
- **CI.** Workflow names are prefixed "Pa Tools"; the reusable workflows upstream calls still have to be
  vendored (`docs/ci-notes-patools.md`).

## Licence requirement: no ArtCraft marks

Upstream's brand terms (`docs/brand/LICENSE-brand.txt` at the fork point) allow the ArtCraft name,
wordmark and logos only in SoundCraft itself. A modified version must remove them and must not be
called ArtCraft or be presented as an ArtCraft product, but may say, in plain text, that it is based on
SoundCraft by the ArtCraft team. So in this repository:

- the eight `docs/brand/artcraft-*` images and `docs/brand/LICENSE-brand.txt` are removed;
- the README, About text, installer links and package metadata carry no ArtCraft logo, Discord link or
  `getartcraft.com` link;
- the credit appears as plain text in `README.md`, `NOTICE`, `ATTRIBUTION.md` and this file.

## Pa Tools' own rules about upstream code

Upstream is the code donor for the base. Pa Tools additionally allows FilmCraft (also by the ArtCraft
team, same licence) as a code donor, and nothing GPL, AGPL or LGPL is ever copied in (`AGENTS.md`).
