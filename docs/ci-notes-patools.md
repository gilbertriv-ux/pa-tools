# CI notes for Pa Tools

**Status: revised 2026-10-23 (rev 2.1), after Pa Tools' own CI workflows ran on GitHub (draft PR #4,
gap G-14) and an independent review of that pull request.** Read before you touch anything under `.github/`. Rev 1 was written on 2026-10-22 at the
fork point (upstream `9b8283b`), before the shared workflow repository could be read or anything was
run; several of its beliefs turned out wrong and are corrected here.

## Revision history

| Date | Rev | What changed |
|---|---|---|
| 2026-10-22 | 1 | Written by the packaging worker at the fork point. The six workflows were thin callers to `storytold/craft-common-actions`, which was believed to be **private** and was not read. Included an untested sketch of a Windows MSI job that lacked the `wix extension add` step. |
| 2026-10-23 | 2 | craft-common-actions read (public, **no licence**, trust and cost findings below). Gil decided to write Pa Tools' own workflows. Added `ci.yml` (PR gate on Linux, build and test on macOS and Windows) and `windows-msi.yml` (unsigned x64 and arm64 MSIs, built and run on GitHub). Replaced the MSI sketch with the real file. Added the failure-annotation scheme and the Dependabot ignore block. Corrected the "private" claim. |
| 2026-10-23 | 2.1 | Review fixes on draft PR #4. **Provenance:** a smoke-test step in the first `windows-msi.yml` was derived from craft-common-actions (no licence); removed and rewritten (see "Provenance incident" below). Pinned the Rust toolchain. Recovery-test note corrected; `leftovers()` keeps a tie; one concurrency group per commit on `pa-tools`; cache saved only from `pa-tools`; Dependabot egui ignores narrowed; NOTICE and licences installed by the MSI. |

## Where things stand

| Workflow | Whose | State |
|---|---|---|
| `ci.yml` (Pa Tools CI) | Pa Tools' own | Runs on every pull request, on pushes to `pa-tools` and `feat/**`, and by hand. |
| `windows-msi.yml` (Pa Tools Windows MSI) | Pa Tools' own | Builds the unsigned x64 and arm64 MSIs; runs on pushes to `pa-tools`, by hand, and on PRs touching `packaging/windows/**`. |
| `speech-real-model.yml` (Pa Tools Speech) | Pa Tools' own | On branch `feat/g18-speech` (PR #2), not on `pa-tools` yet. Self-contained. |
| `release.yml`, `windows-arm64.yml`, `freebsd.yml`, `nix.yml`, `packaging-lint.yml`, `sonarcloud.yml` | thin callers to `craft-common-actions@main` | Unchanged. See "The thin callers" below. |
| `dependabot.yml` | GitHub | Daily, grouped. Ignores the egui family and symphonia 0.6 (see below). |

## What `craft-common-actions` turned out to be (read on 2026-10-23, commit `bfec7e2`)

- **It is public.** Calls from `gilbertriv-ux/pa-tools` start and run: SonarCloud, Packaging lint and
  Nix have all run on pull requests here. Rev 1's "private, every workflow fails at startup" was wrong.
- **It has no licence file.** Cloning and forking are allowed by GitHub's terms; copying its YAML into
  another repository is not licensed. Nothing from it is meant to be copied into this repository; the
  one case where that went wrong, and how it was put right, is under "Provenance incident" below.
- **It has no build-and-test workflow.** It holds `release.yml` (1,192 lines: macOS universal, Windows
  x64/x86/arm64, Linux, Flatpak, FreeBSD VM, Arch, web, riscv64, draft release), `freebsd.yml`,
  `windows-arm64.yml`, `packaging-lint.yml`, `nix.yml`, `sonarcloud.yml`, `supply-chain.yml`,
  `i18n-gate.yml` and 13 composite actions. Each app keeps its own `ci.yml`; upstream SoundCraft has none.
- **Calling it at `@main` hands upstream the keys.** `release.yml` receives the Apple and Windows
  signing material, the notarisation password and the Azure service principal. Pinning the caller to a
  commit does not help: `release.yml` itself calls 24 other `storytold/craft-common-actions/actions/*`
  at `@main` (36 across the repository), so upstream still decides what code runs next to the secrets.
- **`release.yml` needs a paid runner.** The macOS job is `runs-on: macos-15-xlarge`, a larger runner.
  On a free account that job never starts, so nothing downstream of it (the draft release) runs. It
  also fails on the `release` branch when the signing secrets are empty.

## Gil's decision (2026-10-23) and the options that were open

1. **Fork and pin.** Fork craft-common-actions, rewrite every `@main` to the fork at a commit SHA, use
   `macos-15`. Quick, but the fork still contains unlicensed code.
2. **Ask storytold for a licence line.** The clean way to vendor or fork it, but it depends on them.
3. **Write Pa Tools' own workflows.** **Chosen.** Nothing from craft-common-actions is copied. The
   permissively licensed app workflows (FilmCraft, PhotoCraft; MIT OR Apache-2.0) may be adapted, with a
   provenance line in the YAML and a row in `ATTRIBUTION.md`; pinning a public action to its commit SHA
   is just using facts about public tools.

The release workflow (signing, notarisation, draft release) is therefore still to be written; see
"What still has to be written". Until then the six thin callers stay as they are.

## Pa Tools' own workflows

Common to both: `permissions: contents: read`, no secrets, standard free GitHub-hosted runners (no
xlarge), every third-party action pinned to a commit SHA with its version in a comment, Rust installed
with `rustup` at one pinned version (no action for it), `Swatinem/rust-cache` for dependencies, `concurrency` that cancels
superseded runs (but never on `pa-tools`). Pa Tools keeps its own version numbers: nothing here reads or
sets upstream's version; `package.ps1` reads `[workspace.package] version` from this repository's
`Cargo.toml`.

| Action | Pinned to | Version |
|---|---|---|
| `actions/checkout` | `3d3c42e5aac5ba805825da76410c181273ba90b1` | v7.0.1 |
| `Swatinem/rust-cache` | `6323deb102c322ba6fcbdcafc7e3dddab59af2b6` | v2.9.2 |
| `actions/upload-artifact` | `cf430e030ddbb5b0abf93d22962f4752f3646cd9` | v7.0.2 |

All three were checked against the tags on github.com (`git ls-remote`). Dependabot's `github-actions`
ecosystem proposes bumps of these SHAs.

### The Rust toolchain is pinned

Both workflows install one exact Rust release, set once per file in `env: RUST_TOOLCHAIN: "1.97.0"`
(`ci.yml` and `windows-msi.yml`), not GitHub's moving `stable`. Why: on 2026-10-23 `stable` became
Rust/clippy 1.99, which added the lint `chunks_exact_to_as_chunks`; under `clippy -D warnings` that broke
two feature branches whose code builds clean on the developers' machines (`rustc 1.97.0`). A gate that
turns red because the tool changed, not the code, teaches people to ignore it.

- **Upgrading Rust is a deliberate one-line change** in each workflow (the two `RUST_TOOLCHAIN` lines,
  kept equal), made after running the new clippy locally (`cargo clippy --workspace --all-targets -- -D
  warnings` with the new toolchain) and fixing or allowing what it reports, in the same pull request.
- **There is no `rust-toolchain.toml`, on purpose.** Agents' local toolchains cannot download the
  `wasm32-unknown-unknown` target, and a toolchain file that lists it would break their builds. CI names
  the target itself.
- **The MSRV is separate and unchanged:** `rust-version = "1.90"` in the root `Cargo.toml` is the lowest
  compiler the code promises to build with; the pin is what CI builds with.
- `rust-cache` keys its cache on the compiler version, so a new pin starts from a cold cache once.

### `ci.yml` (Pa Tools CI)

- **`gate`** (`ubuntu-24.04`): `cargo xtask ci` = rustfmt check, `clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace`, `assets`, `layers`, and the wasm32 check. The toolchain is the
  pinned Rust release (see "The Rust toolchain is pinned") with clippy, rustfmt and the
  `wasm32-unknown-unknown` target (without the target the wasm step could not run). Apt packages: `libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev libx11-dev
  libfontconfig1-dev mesa-vulkan-drivers pkg-config` (the last-but-one gives the egui render tests a
  software Vulkan adapter).
- **`build-test`** (`macos-15`, `windows-latest`): `cargo build --workspace`, then `cargo test
  --workspace --no-fail-fast`. macOS runs the Apple-only code (`au-host`, CoreAudio); Windows runs the
  Windows-only code (WASAPI, the resource compiler in `apps/soundcraft/build.rs`). The whole workspace
  is tested on both; no scoping was needed.
- **Environment:** `CARGO_PROFILE_DEV_DEBUG=0` (faster compiles and links, a much smaller target
  directory), `CARGO_PROFILE_RELEASE_LTO=false` and `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16` (filmcraft
  measured that thin LTO makes linking hundreds of test binaries take over an hour; shipped binaries
  are built by `release.yml` and `windows-msi.yml` with the real release profile), `CARGO_INCREMENTAL=0`.
- **Triggers:** push to `pa-tools` and `feat/**`, every pull request, `workflow_dispatch`. A branch
  that has an open pull request therefore runs twice (once per event); that is accepted, the two runs
  have separate concurrency groups.
- **Timing** (GitHub-hosted runners): with a warm dependency cache the Linux gate takes about 8
  minutes (`cargo xtask ci` itself 5.6), macOS about 5 and Windows about 6; the first, cold run of each
  took 13 to 15 minutes. `rust-cache` also saves after a failed run, so a red run does not throw the
  compiled dependencies away.
- **Provenance:** the `gate` job is adapted from FilmCraft's `ci.yml`, the OS matrix from PhotoCraft's
  (both MIT OR Apache-2.0); see the comment at the top of the file and `ATTRIBUTION.md`.

### `windows-msi.yml` (Pa Tools Windows MSI)

- Runs `packaging/windows/package.ps1 -Arch x64` and `-Arch arm64` on `windows-latest`; the arm64 build
  is cross-compiled there (the image has the MSVC ARM64 build tools), and `package.ps1` itself checks
  both exes' PE machine type and subsystem. `package.ps1` needed no change.
- Installs WiX as a .NET tool into a folder of its own (`dotnet tool install wix --version 5.0.2
  --tool-path ...`), **without extensions**: `patools.wxs` uses core WiX elements only. If a dialog set
  or a util element is added one day, install the matching extension then.
- **Smoke test of the x64 MSI** (`packaging/windows/smoke-test.ps1`, run by the step "Smoke test the
  MSI", after the MSI has been uploaded). In plain words it:
  1. reads the expected version from Cargo: `cargo metadata --no-deps` lists the workspace packages
     (all inherit `[workspace.package] version`; the step fails if they disagree). `package.ps1` reads
     the same key to build the MSI and `patools.wxs` takes `Version` from it (`-d Version=`); no version
     is typed anywhere else, so the test compares what two readers of one key produced;
  2. opens the MSI's database through the `WindowsInstaller.Installer` object and asserts that
     `ProductVersion` is that version (a pre-release tag such as `-rc.1` is dropped from an MSI
     version), the file name carries it, and the File table holds the notice and licence files;
  3. installs it with `msiexec /i ... /qn /norestart /l*v`, under a 10-minute limit;
  4. finds `pa-tools.exe` through the App Paths key the MSI writes, checks it is in `C:\Program Files\Pa
     Tools`, that both exes, `NOTICE`, `LICENSE-MIT`, `LICENSE-APACHE` and `ATTRIBUTION.md` are there
     (the last four byte-identical to the repository's), and that one "Pa Tools" entry with that version
     is listed under Uninstall;
  5. runs `pa-tools.exe --version` and `pa-tools-cli.exe --version` (both print the name and the Cargo
     version) and requires "Pa Tools" and exactly that version;
  6. uninstalls with `msiexec /x ... /qn` and checks the install folder, the App Paths key and the
     Uninstall entry are gone.
  On failure it writes one error annotation (readable through the check-runs API) and the step uploads
  the msiexec logs and captured output as the artifact `patools-windows-x64-msi-smoke-logs`; on success
  it writes one notice annotation with the MSI size and the files in it. The arm64 MSI cannot run on an
  x64 runner; installing it on a Windows 11 ARM64 runner is what the thin caller `windows-arm64.yml`
  does.
- **The MSI installs the notice.** Pa Tools links Symphonia (MPL-2.0); section 3.2 of the MPL asks that
  a recipient of the executable form is told the source is available and where. `NOTICE` says so, and
  `patools.wxs` now installs `NOTICE`, `LICENSE-MIT`, `LICENSE-APACHE` and `ATTRIBUTION.md` next to the
  exes (the files the portable zip already carries); `package.ps1` passes the repository root as
  `LegalDir`, and a missing file fails `wix build`.
- Unsigned: no signing secret is passed in, so `sign.ps1` prints its "leaving unsigned" warning and
  exits 0. Signed installers are the release workflow's job.
- Artifacts (14 days): `patools-windows-x64-msi` (holds `patools-0.11.0-windows-x64.msi`; the artifact
  is 11.4 MB) and `patools-windows-arm64-msi` (holds `patools-0.11.0-windows-arm64.msi`; 10.2 MB). The two portable zips that
  `package.ps1` also makes are not uploaded.
- Timing: about 6 to 7 minutes per architecture warm (cold: 6 for x64, 10 for arm64).

### Provenance incident (review of draft PR #4, fixed in the same pull request)

An independent review found that about 14 lines of the first `windows-msi.yml`, in its smoke-test step,
were derived from `storytold/craft-common-actions`, which has no licence file. The header of that file
said nothing had been copied.

- **Removed** in commit `ab50b38` (that step, and the `wix extension add` lines, which `patools.wxs`
  does not need); the WiX tool is installed without extensions (commit `b598957`).
- **Rewritten** as `packaging/windows/smoke-test.ps1`, written for Pa Tools from public documentation
  only (Microsoft Learn on msiexec, the Windows Installer automation objects and the Uninstall registry
  key; the GitHub Actions workflow-command docs) and from this repository's own `patools.wxs` and
  `package.ps1`. It was written without opening craft-common-actions or the removed lines.
- **History:** Gil approved a **squash merge** of the pull request, so the removed lines never enter
  `pa-tools` history. The `ci-probe/*` branches used while testing are deleted only after the squash
  merge has landed.
- The headers of `windows-msi.yml` and the `ATTRIBUTION.md` rows say this, briefly.

### Reading a failed run when the job log is out of reach

Job logs are served from a separate blob host that some sandboxes and proxies cannot reach.
`.github/scripts/ci-digest.sh` therefore turns the captured log of a failed step into **one error
annotation** (compiler errors, failing tests, panics, last lines). It is on the run's summary page and
on the PR, and readable through the API without the log:

```sh
gh api repos/gilbertriv-ux/pa-tools/actions/runs/<run-id>/jobs --jq '.jobs[] | "\(.id) \(.name) \(.conclusion)"'
gh api repos/gilbertriv-ux/pa-tools/check-runs/<job-id>/annotations --jq '.[] | .title, .message'
```

`cargo xtask ci` stops at its first failing step and `cargo test` at the first failing test binary, so
when the gate fails in its test step, a second step (only on failure) runs `cargo test --workspace
--no-fail-fast` and the `assets`, `layers` and `wasm` gates and reports everything that is red in one
annotation. The macOS/Windows job always uses `--no-fail-fast`.

### Things the first runs found

Putting the workspace tests on Linux and Windows for the first time showed that some tests had only
ever run on a Mac:

- **Windows:** `io::save_tests::saved_audio_is_trusted_only_in_the_folder_it_was_saved_in`
  (`crates/engine/src/io.rs`) built its expected path with a `/` inside one `join()`, which differs
  from the backslash path the engine returns. Test-only fix: join the components one by one.
- **Linux:** the recovery tests (`crates/ui-egui/tests/recovery.rs`) failed at random, a different one
  on each run, with two real causes (1 and 3) and one defensive change (2):
  1. *Product bug (data loss), found by this flake:* an autosave counted as unsaved work only if it
     was strictly newer than the session file it came from. Linux file systems stamp files from a clock
     that ticks every few milliseconds, so a Save and the autosave right after it could carry the same
     time and `leftovers()` deleted the autosave as outdated. Macs with HFS+ (one-second times) and FAT
     volumes (two seconds) have the same problem with any edit made within a tick of a Save, so after a
     crash the edit was lost. `leftovers()` now keeps an autosave whose time equals the session file's
     (`saved >= o`), with a regression test that sets both times equal. The test's 50 ms pause between
     the save and the edit stays, so the edit is strictly newer, as it is for a user.
  2. *Test (defensive, not a cause):* a test counted the autosave folders right after
     `SoundApp::recover()`. That call is synchronous (`crates/ui-egui/src/lib.rs`: `recover()` calls
     `write_autosave()`, which flushes the background writer before it returns), so the folder is
     already there. The test now waits for it with the file's own `eventually()` anyway, so it does not
     depend on that.
  3. *Product bug (data loss):* an autosave folder was named `<name> <epoch ms>-<pid>` and made with
     `create_dir_all`, so when one session closed (its folder kept for the next launch, holding unsaved
     work) and the next opened within the same millisecond, the new autosave reused the kept folder and
     overwrote its `session.scraft`; the folder was then locked, so the next launch could not offer it.
     `Autosave::create` (`crates/engine/src/autosave.rs`) now uses `create_dir` and gives a name that
     already exists a numeric suffix, with a regression test that makes 50 folders back to back.
  After the fixes the recovery test binary passed 20 of 20 runs on the runner (10 normal, 10 pinned to
  one CPU); before, about half of the runs failed.
- **`ATTRIBUTION.md`:** the Pa Tools screenshots row listed `mix.jpg` and `about.jpg` without their
  folder, so `cargo xtask assets` (and so `cargo xtask ci`) failed on `pa-tools` itself.

Upstream SoundCraft has no `ci.yml` either, which is why none of this had been seen. The engine and
ui-egui changes are separate commits (test-only, apart from the autosave fix) so the lane holders of
those crates can carry or drop them.

## The thin callers (unchanged)

| Workflow (name) | Runs when | What it does | Needs |
|---|---|---|---|
| `release.yml` (Pa Tools Release) | push to `release`; manual run with an optional `version` | Builds every platform's installer, signs the macOS and Windows ones, drafts a GitHub Release `Pa Tools v<version>` with `SHA256SUMS.txt` | the 14 signing secrets below (all optional), a `release` environment, and **a paid `macos-15-xlarge` runner** |
| `windows-arm64.yml` (Pa Tools Windows ARM64) | nightly 05:53 UTC; push to `release`; PRs touching `packaging/windows/**` | Cross-compiles the ARM64 MSI on an x64 runner (unsigned), installs it on a Windows 11 ARM64 runner (`windows-11-arm`), checks the installed binaries are ARM64, runs the CLI natively, uninstalls | none |
| `freebsd.yml` (Pa Tools FreeBSD) | nightly 03:47 UTC; push to `release`; PRs touching `crates/playback`, `packaging/freebsd/**`, `packaging/env.sh` | Builds and tests the workspace inside a FreeBSD 14.3 VM on a Linux runner | none |
| `nix.yml` (Pa Tools Nix) | push to `main` and PRs touching `flake.nix`, `flake.lock`, `nix/**`, `Cargo.lock` | Evaluates the Nix flake only. This tree has no `flake.nix` or `nix/` directory; it has run (green) on PRs that changed `Cargo.lock`. Delete it or add the flake | none |
| `packaging-lint.yml` (Pa Tools Packaging lint) | push to `main`/`release` and PRs touching workflows, `packaging/**`, `assets/app-icon/**` | actionlint, shellcheck, PowerShell parse, xmllint (WiX, plists, AppStream, MIME), `desktop-file-validate`, `appstreamcli validate`, Flatpak manifest check. It also lints Pa Tools' own workflows, and is green on this PR | none |
| `sonarcloud.yml` (Pa Tools SonarCloud) | push to `main`; PRs | Static analysis on SonarCloud | `SONAR_TOKEN` and a SonarCloud organisation (`sonar-project.properties`: `gilbertriv-ux_pa-tools`) |
| `dependabot.yml` | daily 06:00 America/New_York | Grouped PRs for Cargo and GitHub Actions | Dependabot enabled on the repository |

Three of those (`nix.yml`, `packaging-lint.yml`, `sonarcloud.yml`) trigger on pushes to `main`, but the
integration branch and the repository's default branch is `pa-tools`: those push triggers never fire
here. They still run on pull requests. Adjust them when the callers are replaced.

### Dependabot

The Cargo updates ignore major and minor bumps of `egui`, `eframe`, `egui_kittest`, `egui_*`, `epaint`,
`emath` and `ecolor` (craft-ui, pinned in the root `Cargo.toml`, is built against egui 0.36, so the
family moves only after craft-libs does; 0.36.x patch releases still come through) and of `symphonia*`
(0.6 breaks the API; 0.5.x patch releases still come through). Lift the egui block when the craft-libs
pin moves.

## What still has to be written

In priority order; everything is Pa Tools' own code, pinned by commit SHA.

1. ~~Windows MSI, x64 and arm64~~ and ~~build and test~~: done (`windows-msi.yml`, `ci.yml`).
2. **Windows ARM64 install check** on `windows-11-arm`: download the arm64 MSI artifact, `msiexec /i
   ... /qn`, check the installed exes' PE machine type, run `pa-tools-cli.exe --version`, uninstall.
   (Check that the free plan offers that label for this repository.) Replaces the thin caller.
3. **Linux**: x86_64 on `ubuntu-22.04`, aarch64 on `ubuntu-22.04-arm`; `packaging/linux/package.sh` makes
   the AppImage, `.deb`, `.rpm` (nfpm) and tarball; `flatpak-bundle.sh` repackages the tarball
   (`ubuntu-24.04` and `ubuntu-24.04-arm`).
4. **macOS**: `packaging/macos/package.sh` on `macos-15` (a free runner, not xlarge). Needs Gil's own
   Developer ID certificate; until the secrets exist it signs ad hoc.
5. **Web**: `packaging/web/package.sh` (Trunk) on `ubuntu-24.04`.
6. **Packaging lint**: `actionlint`, `shellcheck`, `pwsh` parse, `xmllint`, `desktop-file-validate`,
   `appstreamcli validate`. No secrets, cheap. (`actionlint` and `shellcheck` already pass locally on
   the new workflows.)
7. **FreeBSD VM**, **riscv64** (cross-compiled, QEMU smoke test), **SonarCloud**: last. None blocks a
   release of the desktop installers.
8. **The draft-release job** that gathers the artifacts, writes `SHA256SUMS.txt` and creates the draft.
   Only this job gets `contents: write`, and only it and the signing jobs declare `environment: release`.
   Keep signing inside `sign.ps1` and the macOS script as now.

Add the fonts and i18n checkouts to `ci.yml` when localisation (G-09) lands and a `CRAFT_*` variable
appears; Pa Tools uses none yet.

## Secrets and settings to create (Needs Gil)

All optional; each missing one only leaves that platform unsigned. Put them in the repository's `release`
environment (*Settings, Environments, release*), restricted to the `release` branch. Never commit them.

| Secret | For |
|---|---|
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD` | base64 `.p12` of Gil's own "Developer ID Application" certificate (needs a paid Apple Developer account) and the temporary keychain |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | notarisation (`notarytool`) |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | base64 `.pfx` code-signing certificate (the simplest option) |
| `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_CERT_PROFILE` | Azure Trusted Signing, an alternative to the `.pfx` |
| `SONAR_TOKEN` | SonarCloud (organisation and project `gilbertriv-ux_pa-tools` must exist) |

Also placeholders to confirm before a public release: the repository name `gilbertriv-ux/pa-tools` (in the
README downloads table, the AppStream metainfo and the AppImage update information), the homepage
`https://gr8idyuz.ai`, and the maintainer address `patools@gr8idyuz.ai` in the Linux packages. The full list is in
`packaging/README-PATOOLS.md`.

## Until the release workflows exist

Build locally with the `packaging/` scripts (the README lists the commands per platform). A push to
`release` still runs the thin `release.yml` caller, which needs the paid macOS runner and so does not
get as far as drafting a release. For a Windows installer without a local Windows machine, run *Actions,
Pa Tools Windows MSI, Run workflow* and download the artifacts.
