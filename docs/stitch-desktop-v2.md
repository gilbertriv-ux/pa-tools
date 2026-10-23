# Desktop workspace V2

Last updated: 2026-10-22. Revision: initial native implementation and validation contract.

The V2 desktop interface applies the user's design study to the existing Rust/egui app.
The audio engine, session format, speech public API, plugin hosts and command IDs are retained.

## Google tool contributions

- [Stitch project](https://stitch.google.com/projects/13723414442799651417): generated an additional
  **PA Tools Desktop V2** frame at 1440 × 1000, preserving the previous frames. Its useful inputs
  are the dark studio palette, a session header, explicit Edit/Mix/Transcript navigation,
  empty-session actions and a substantial transcript inspector with speaker cards.
- [Opal workflow](https://opal.google/edit/1hu6KhvsJaGaBZZz6r0refx_XSZXXrbgR): ran the V2 planning
  brief and returned six phases: theme, workspace/header, mixer, timeline, transcript and QA.
  Its report also displays a Desktop/file-write success banner. That banner is **not** accepted
  as evidence of a Mac file operation or a tested build; its own metadata names a cloud `/mnt/`
  sandbox. Native build and test records are the validation evidence.
- Google AI Studio, account shown as Ultra, Gemini 3.8 Flash: generated original Rust under the
  requested MIT OR Apache-2.0 terms for transcript grouping. The response was cut off during
  its test table. The integrated version uses the existing model type, checked iteration rather
  than direct indexing, saturating timestamp differences, and preserves all words at zero rate.
  Local tests, rather than the generated response, establish behavior.

No exported HTML, remote logos, icon fonts, web dependencies, proprietary product assets or
sample telemetry are bundled. Fonts and procedural icons retain their existing provenance.

## Native implementation

- `crates/ui-egui/src/workspace.rs`: session name, real track count and sample rate, workspace
  navigation and existing menu-style add/import/open/save actions. An empty session now explains
  its next steps and opens the existing dialogs.
- `theme.rs`, `mix_window.rs`: dark surfaces and green/cyan/amber accents, clearer strip boundaries
  and channel color rails. Light appearance and existing audio/mixer behavior remain available.
- `transcript_window.rs`: resizable right inspector on sufficiently wide desktops, detachable
  floating window, small-window fallback, utterance cards, search counts and context, and real
  speaker/timing information. Clickable words retain their exact integer session sample positions.
  The existing background worker and session-revision fence remain in place.

Utterances split on speaker changes, gaps greater than 1.5 seconds, spans greater than 15 seconds,
40-word limits, or backwards start times. Grouping never reorders or drops word indices.

`window.transcript` accepts optional boolean `docked` in addition to its existing `value` toggle.
The host choice is UI preference data and does not edit the document. Hide Floating Windows also
closes this inspector, consistent with its earlier Transcript behavior.

## Validation contract

Run workspace CI (formatting, clippy, tests, assets, layers, WASM), speech-feature clippy/tests,
and the model-backed Transcript button test with real model/fixture paths. Inspect native or
offscreen screenshots of empty, Edit, Mix and Transcript views. Save and reopen a synthetic
session using the packaged native app and discover the speech model from its bundle.

The application remains pre-alpha. This revision does not complete hardware I/O, interchange,
plugin UI depth, or the other readiness gates in `gaps.md`.
