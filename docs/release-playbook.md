# Release playbook

Pa Tools has no separate shared playbook: the release procedure is [releasing.md](releasing.md).
It builds on the playbook of the project Pa Tools is based on (upstream SoundCraft, whose own
playbook lives in a repository that is not part of this tree).

The Pa Tools specifics, in short:

- Artifacts are built by the scripts in `packaging/` (macOS, Windows, Linux, FreeBSD, web); the
  GitHub workflows are thin callers of reusable workflows that still have to be vendored
  ([ci-notes-patools.md](ci-notes-patools.md)).
- The version lives in one place, `[workspace.package] version` in the root `Cargo.toml`
  (`cargo xtask version`).
- The brand (name, ids, binaries, vendor, homepage) is set in `rebrand.sh` and `packaging/env.sh`.
- Signing material is Gil's own and is never committed (macOS Developer ID and notarization
  credentials, Windows certificate or Azure Trusted Signing); without it the builds are ad-hoc
  signed on macOS and unsigned on Windows, which is fine for local use.
