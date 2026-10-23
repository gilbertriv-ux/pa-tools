//! Pa Tools' embedded translation catalog (gap G-09).
//!
//! The ten locale catalogs under `catalog/` are compiled in with `include_str!`. The donor runtime
//! left embedding to the app (a `build.rs` reading `CRAFT_I18N_DIR`); Pa Tools ships exactly one
//! catalog, so it embeds its own directly, with no build script and no environment variable. The
//! translations are the donor craft-i18n's, reused only where the English source text matched
//! exactly; see `docs/i18n.md` and `ATTRIBUTION.md`.

use crate::{Embedded, I18n};

/// The embedded catalogs: `(locale, shared catalog, app catalog)`. Pa Tools is one app, so every
/// string lives in the shared slot and the app slot is empty.
pub static CATALOG: &Embedded = &[
    ("ar", include_str!("../catalog/ar/common.tsv"), ""),
    ("de", include_str!("../catalog/de/common.tsv"), ""),
    ("es", include_str!("../catalog/es/common.tsv"), ""),
    ("fr", include_str!("../catalog/fr/common.tsv"), ""),
    ("it", include_str!("../catalog/it/common.tsv"), ""),
    ("ja", include_str!("../catalog/ja/common.tsv"), ""),
    ("pt-br", include_str!("../catalog/pt-br/common.tsv"), ""),
    ("ru", include_str!("../catalog/ru/common.tsv"), ""),
    ("zh-hans", include_str!("../catalog/zh-hans/common.tsv"), ""),
    ("zh-hant", include_str!("../catalog/zh-hant/common.tsv"), ""),
];

/// The locales Pa Tools offers in its language picker. Its interface must be ready for each: Arabic
/// is right to left, so the app lays text out accordingly (see [`I18n::rtl`]). A locale that is in
/// [`CATALOG`] but not here would still serve as a fallback.
pub const OFFERED: &[&str] = &["ar", "de", "es", "fr", "it", "ja", "pt-br", "ru", "zh-hans", "zh-hant"];

/// The canonical English source strings the catalog translates (gap G-09), one per line, with `#`
/// comments. These are the keys a later lane routes the UI's literals through; the consistency test
/// checks that no catalog row carries a source that is not one of them.
pub const KEYS: &str = include_str!("../catalog/KEYS.txt");

/// Build the Pa Tools [`I18n`] from the embedded catalog, offering [`OFFERED`]. Returns the runtime
/// and any catalog problems for the caller to log; there should be none (the consistency test
/// checks that), but reading a newer, slightly broken catalog must never stop the app from starting.
pub fn pa_tools() -> (I18n, Vec<String>) {
    I18n::new(CATALOG, OFFERED)
}

/// The canonical English keys as a slice (comment and blank lines dropped).
pub fn keys() -> Vec<&'static str> {
    KEYS.lines().map(str::trim_end).filter(|l| !l.is_empty() && !l.starts_with('#')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use crate::Lang;

    #[test]
    fn catalog_is_clean() {
        // The authoritative consistency check: the runtime reads the real catalog with each
        // locale's own plural-form count and reports every malformed or empty row. There must be
        // none, so the embedded catalog is well formed and has no empty values.
        let (_i18n, problems) = pa_tools();
        assert!(problems.is_empty(), "embedded catalog problems: {problems:?}");
    }

    #[test]
    fn keys_file_lists_forty() {
        assert_eq!(keys().len(), 40, "KEYS.txt should list the 40 catalogued strings");
    }

    #[test]
    fn every_plain_source_is_a_known_key() {
        let known: BTreeSet<&str> = keys().into_iter().collect();
        for (code, shared, _app) in CATALOG {
            // Plain rows parse regardless of the plural-form count passed here; @plural rows that
            // don't match it are simply dropped, which is fine because this test only inspects
            // plain rows (catalog_is_clean validates @plural rows with the real counts).
            let (entries, _problems) = crate::catalog::parse(shared, 2);
            for e in &entries {
                if e.context.is_empty() {
                    assert!(known.contains(e.source.as_str()), "{code}: catalog row for unknown key {:?}", e.source);
                    assert!(!e.translation.is_empty(), "{code}: empty translation for {:?}", e.source);
                }
            }
        }
    }

    #[test]
    fn english_is_offered_first_then_the_ten() {
        let (i18n, _) = pa_tools();
        let codes: Vec<&str> = i18n.languages().iter().map(|&l| i18n.code(l)).collect();
        assert_eq!(codes.first(), Some(&"en"));
        let offered: BTreeSet<&str> = codes.iter().skip(1).copied().collect();
        assert_eq!(offered, OFFERED.iter().copied().collect::<BTreeSet<_>>());
        assert!(!i18n.languages().contains(&Lang::PSEUDO));
    }

    #[test]
    fn every_locale_names_itself() {
        let (i18n, _) = pa_tools();
        for &code in OFFERED {
            let Some(lang) = i18n.from_code(code) else {
                panic!("offered locale {code} is not usable");
            };
            // A locale with no `@id language.name` row falls back to its code.
            assert_ne!(i18n.name(lang), code, "{code} has no language.name row");
        }
    }

    #[test]
    fn real_translations_are_reused() {
        let (i18n, _) = pa_tools();
        // Spot-check a few looked-up strings against the donor's reused translations.
        let fr = i18n.from_code("fr").unwrap_or(Lang::EN);
        assert_eq!(i18n.t(fr, "Save"), "Enregistrer");
        assert_eq!(i18n.t(fr, "Cancel"), "Annuler");
        // Fallback: a string not in the catalog stays English.
        assert_eq!(i18n.t(fr, "Quantize to grid"), "Quantize to grid");
    }

    #[test]
    fn pseudo_wraps_every_catalogued_string() {
        // Every one of the 40 catalogued strings comes back pseudo-localised, so a string drawn
        // without the brackets is one that bypassed translation. (The pseudo locale is debug-only;
        // tests run in debug.)
        let (i18n, _) = pa_tools();
        for key in keys() {
            let got = i18n.t(Lang::PSEUDO, key);
            assert!(got.starts_with("[!! ") && got.ends_with(" !!]"), "{key:?} -> {got:?}");
            assert_ne!(got, key, "{key:?} was not transformed by the pseudo locale");
        }
    }

    #[test]
    fn malformed_input_is_reported_not_panicked() {
        // Errors are values: a broken catalog yields problems and still builds a usable runtime.
        let bad: &Embedded = &[("fr", "only two\tcolumns\n@bogus\tx\ty\n\tFoo\t\n", ""), ("de", "\u{feff}\tBar\tBaz\n", "")];
        let (i18n, problems) = I18n::new(bad, &["fr", "de"]);
        assert!(!problems.is_empty(), "expected problems from the malformed catalog");
        assert_eq!(i18n.t(Lang::EN, "Save"), "Save");
        assert_eq!(i18n.t(i18n.from_code("de").unwrap_or(Lang::EN), "Bar"), "Baz");
    }
}
