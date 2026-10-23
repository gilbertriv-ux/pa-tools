# soundcraft-i18n

The localisation runtime for Pa Tools (layer 0, standalone). It looks English source text up in a
catalog, with fallback to English, plurals, language names, the system locale and a debug-only
pseudo locale. A missing translation is the English itself; nothing here panics.

This crate was imported from the `i18n` crate of
[storytold/craft-libs](https://github.com/storytold/craft-libs) (MIT OR Apache-2.0) and adapted for
Pa Tools; the embedded translations come from
[storytold/craft-i18n](https://github.com/storytold/craft-i18n) (MIT OR Apache-2.0). See
`docs/i18n.md` for the design, the donor commit SHAs and how a later lane migrates the UI's strings,
and `ATTRIBUTION.md` for the donor rows.

## What the UI lane will do

Nothing in `crates/ui-egui`, `crates/engine` or `crates/mix` uses this crate yet; the string
migration is a separate lane. When it lands, the UI will:

```rust
use soundcraft_i18n as i18n;

// Once, at start-up: pick the language the user asked for.
i18n::use_system_lang();              // or i18n::set_lang_code("fr")

// Everywhere a literal is shown today:
ui.button(i18n::tr!("Save"));         // plain
i18n::trc!("menu", "Open");           // with a context
i18n::trn!(n, "{n} item", "{n} items"); // a count
```

The language picker reads `i18n::global().languages()` and `i18n::global().name(lang)`; switching is
`i18n::set_lang(lang)` and takes effect on the next frame. See `docs/i18n.md`.
