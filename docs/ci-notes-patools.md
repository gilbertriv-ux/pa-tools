# CI notes for Pa Tools

**Status: written 2026-10-22, at the fork point (upstream `9b8283b`).** Read before the first push to
GitHub account `gilbertriv-ux`. The workflow files under `.github/workflows/` were renamed ("Pa Tools
...", `app-name: patools`, `app-title: Pa Tools`, `app-dir: Pa Tools`, SonarCloud key
`gilbertriv-ux_pa-tools`), but they are still **thin callers**. Nearly all of the build logic lives in
reusable workflows of upstream's organisation, which are not in this tree:

```
uses: storytold/craft-common-actions/.github/workflows/<name>.yml@main
```

I (the packaging worker) did not read those reusable workflows: the repository was not reachable from
the session. What follows about them comes from this tree: the callers, `docs/releasing.md`,
`packaging/**` and the comments in the workflow files. Check it against the real files when you vendor them.

## What each workflow does

| Workflow (name) | Runs when | What it does | Calls | Needs |
|---|---|---|---|---|
| `release.yml` (Pa Tools Release) | push to `release`; manual run with an optional `version` | Builds every platform's installer, signs the macOS and Windows ones, drafts a GitHub Release `Pa Tools v<version>` with `SHA256SUMS.txt` | `craft-common-actions/.../release.yml@main`, inputs `app-name: patools`, `app-title: Pa Tools`, `version` | the 14 signing secrets below (all optional), a `release` environment |
| `windows-arm64.yml` (Pa Tools Windows ARM64) | nightly 05:53 UTC; push to `release`; PRs touching `packaging/windows/**` | Cross-compiles the ARM64 MSI on an x64 runner (unsigned), installs it on a Windows 11 ARM64 runner, checks the installed binaries are ARM64, runs the CLI natively, uninstalls | `.../windows-arm64.yml@main`, inputs `app-name: patools`, `app-dir: Pa Tools` | none |
| `freebsd.yml` (Pa Tools FreeBSD) | nightly 03:47 UTC; push to `release`; PRs touching `crates/playback`, `packaging/freebsd/**`, `packaging/env.sh` | Builds and tests the workspace inside a FreeBSD 14.3 VM on a Linux runner (GitHub has no FreeBSD runners) | `.../freebsd.yml@main` | none |
| `nix.yml` (Pa Tools Nix) | push to `main` and PRs touching `flake.nix`, `flake.lock`, `nix/**`, `Cargo.lock` | Evaluates the Nix flake only (about a minute, no build) so a `Cargo.lock` change that `nix/package.nix` cannot vendor is noticed. **This tree has no `flake.nix` or `nix/` directory**, so the workflow never triggers and has nothing to evaluate; delete it or add the flake | `.../nix.yml@main` | none |
| `packaging-lint.yml` (Pa Tools Packaging lint) | push to `main`/`release` and PRs touching workflows, `packaging/**`, `assets/app-icon/**` | actionlint, shellcheck, PowerShell parse, xmllint (WiX, plists, AppStream, MIME), `desktop-file-validate`, `appstreamcli validate`, and a check that the two Flatpak manifests agree | `.../packaging-lint.yml@main` | none |
| `sonarcloud.yml` (Pa Tools SonarCloud) | push to `main`; PRs | Static analysis on SonarCloud | `.../sonarcloud.yml@main`, `secrets: inherit` | `SONAR_TOKEN` and a SonarCloud organisation (`sonar-project.properties`: `gilbertriv-ux_pa-tools`, org `gilbertriv-ux`) |
| `dependabot.yml` | daily 06:00 America/New_York | Opens grouped PRs for Cargo and GitHub Actions updates | built into GitHub | enabling Dependabot on the repository |

**There is no build-and-test workflow in this tree.** Upstream's `cargo xtask ci` (fmt, clippy with
`-D warnings`, tests, assets, layers, wasm check) is not run by any file here. Add one, because
`AGENTS.md` makes that command the gate before every commit.

## Why they have to be vendored

1. **Cross-account reuse.** A workflow in `gilbertriv-ux/pa-tools` can call
   `storytold/craft-common-actions` only if that repository is public (or shared with the account).
   If it is private, every one of these workflows fails at startup.
2. **Trust.** The callers track `@main`, "so org-wide CI improvements ... propagate". That means anyone
   who can push to upstream's `main` changes the code that runs with Gil's signing secrets. The
   `release` workflow passes the Apple and Windows certificates, the notarisation password and the Azure
   service principal to it. Pa Tools should run workflows that live in its own repository, pinned by commit.
3. **Upstream identity.** The reusable workflows were written for upstream's apps; they may carry its
   names, release-notes wording or runner labels (larger or paid runners) that `gilbertriv-ux` may not have.
   Compare the `runs-on:` lines with your plan before the first run.

## What to vendor, in priority order

Replace each `uses:` with jobs written in the same file, or copy the reusable workflows into
`.github/workflows/` here (as `workflow_call` files) and change each `uses:` to `./.github/workflows/<name>.yml`.
Pin every third-party action by commit SHA.

1. **Windows MSI, x64 and arm64 (first).** Everything the job needs is already in `packaging/windows/`.
   - Runner: `windows-latest` (the ARM64 MSI is cross-compiled there; upstream's `windows-arm64.yml` then installs it on a
     Windows 11 ARM64 runner).
   - Steps: checkout; Rust stable with the MSVC target (`x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`); make sure the
     MSVC ARM64 build tools are installed for the arm64 build; `dotnet tool install --global wix --version 5.0.2`;
     then `pwsh packaging/windows/package.ps1 -Arch x64` (and `-Arch arm64`); upload `dist/release/*`.
   - Set `SOUNDCRAFT_REQUIRE_WINRES=1` (a missing resource compiler must fail the build, not ship an icon-less exe).
   - Signing is already inside `package.ps1` through `sign.ps1`; it skips with a warning when no secrets are set, so
     the first builds are unsigned and still valid installers.
   - Then a `windows-arm64` job: download the arm64 MSI, `msiexec /i ... /qn` on a Windows ARM64 runner (label `windows-11-arm`; check that your plan offers it for this repository), run
     `"C:\Program Files\Pa Tools\pa-tools-cli.exe" --version`, check the PE machine type, uninstall.
2. **Build and test (`cargo xtask ci`)** on `ubuntu-22.04` (needs the audio and X11/Wayland dev packages, see
   `docs/releasing.md`, "Audio libraries"), triggered on push and pull requests.
3. **Packaging lint.** The checks are listed in the table. They need no secrets and are cheap; a plain job with
   `actionlint`, `shellcheck`, `pwsh` parse, `xmllint`, `desktop-file-validate` and `appstreamcli validate` is enough.
4. **Linux**: x86_64 on `ubuntu-22.04`, aarch64 on `ubuntu-22.04-arm`. `packaging/linux/package.sh` makes the
   AppImage, `.deb`, `.rpm` (nfpm) and tarball; `flatpak-bundle.sh` repackages the tarball into a `.flatpak`
   (`ubuntu-24.04` and `ubuntu-24.04-arm`).
5. **macOS**: `packaging/macos/package.sh` on `macos-15` (add `--arch x86_64` for an Intel build, or build a universal
   binary). Needs Gil's own Developer ID certificate; until the secrets exist it signs ad-hoc (see "Needs Gil" below).
6. **Web**: `packaging/web/package.sh` (Trunk) on `ubuntu-latest`.
7. **FreeBSD VM**, **riscv64** (cross-compiled, QEMU smoke test), **SonarCloud**: last. (Nix: only if a flake is added.) None blocks a release of
   the desktop installers.
8. **The draft-release job** that gathers the artifacts, writes `SHA256SUMS.txt` and creates the draft. Only this job gets
   `contents: write`, and only it and the signing jobs declare `environment: release`.

### Sketch of the first job (not tested)

A starting point for `.github/workflows/windows-msi.yml`. It was written from `package.ps1`, not run, so expect to adjust it.

```yaml
name: Pa Tools Windows MSI
on:
  workflow_dispatch:
  push:
    branches: [release]
permissions:
  contents: read
jobs:
  msi:
    runs-on: windows-latest
    strategy:
      matrix:
        arch: [x64, arm64]
    env:
      SOUNDCRAFT_REQUIRE_WINRES: "1"
    steps:
      - uses: actions/checkout@<pin a commit sha>
      - uses: dtolnay/rust-toolchain@<pin a commit sha>
        with:
          toolchain: stable
          targets: ${{ matrix.arch == 'arm64' && 'aarch64-pc-windows-msvc' || 'x86_64-pc-windows-msvc' }}
      - run: dotnet tool install --global wix --version 5.0.2
      - run: pwsh packaging/windows/package.ps1 -Arch ${{ matrix.arch }}
      - uses: actions/upload-artifact@<pin a commit sha>
        with:
          name: patools-windows-${{ matrix.arch }}
          path: dist/release/*
```

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

## Until the workflows exist

Build locally with the `packaging/` scripts (the README lists the commands per platform). Nothing in a push
to `release` builds an installer until item 1 above is in place.
