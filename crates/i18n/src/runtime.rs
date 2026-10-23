//! A process-wide [`I18n`] built once from Pa Tools' embedded catalog, the current interface
//! language, and the helpers the [`tr!`](crate::tr) family of macros use. The UI looks a string up
//! without threading the runtime and the language through every call; changing the language with
//! [`set_lang`] takes effect on the next lookup, which is how Pa Tools switches language while it
//! runs.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{I18n, Lang};

/// The catalog and the problems found reading it, built once on first use.
static GLOBAL: OnceLock<(I18n, Vec<String>)> = OnceLock::new();
/// The current language as a [`Lang`] index; `0` is [`Lang::EN`], the default until the app sets it.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

fn built() -> &'static (I18n, Vec<String>) {
    GLOBAL.get_or_init(crate::pa_tools)
}

/// The process-wide catalog, built once from Pa Tools' embedded catalog.
pub fn global() -> &'static I18n {
    &built().0
}

/// Any problems found while reading the embedded catalog (there should be none). An app may log
/// them once at start-up.
pub fn problems() -> &'static [String] {
    &built().1
}

/// The current interface language (English until [`set_lang`], [`set_lang_code`] or
/// [`use_system_lang`] is called).
pub fn current_lang() -> Lang {
    Lang(CURRENT.load(Ordering::Relaxed))
}

/// Switch the interface language. Takes effect on the next lookup. `lang` must come from this
/// crate (e.g. [`global`]`().from_code(..)`, [`global`]`().system_lang()`, or [`Lang::EN`]).
pub fn set_lang(lang: Lang) {
    CURRENT.store(lang.0, Ordering::Relaxed);
}

/// Switch to the language with this code (an offered locale, or `en`). Returns `false` and keeps
/// the current language when the code is not one the app offers.
pub fn set_lang_code(code: &str) -> bool {
    match global().from_code(code) {
        Some(lang) => {
            set_lang(lang);
            true
        }
        None => false,
    }
}

/// Switch to the language the system asks for, or English if none of the offered locales fits.
pub fn use_system_lang() {
    set_lang(global().system_lang());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All of the global-language mutation lives in this one test, so the process-wide `CURRENT`
    /// is never raced by another test (every other test passes an explicit `Lang`).
    #[test]
    fn switching_language_and_macros() {
        // Problems reading the embedded catalog: there should be none.
        assert!(problems().is_empty(), "{:?}", problems());

        // Default is English.
        set_lang(Lang::EN);
        assert_eq!(current_lang(), Lang::EN);
        assert_eq!(crate::tr!("Save"), "Save");

        // Switch at runtime: the same lookup now returns the French translation.
        assert!(set_lang_code("fr"));
        assert_eq!(current_lang(), global().from_code("fr").unwrap());
        assert_eq!(crate::tr!("Save"), "Enregistrer");
        assert_eq!(crate::tr!("Cancel"), "Annuler");

        // And German.
        assert!(set_lang_code("de"));
        assert_eq!(crate::tr!("Save"), global().t(global().from_code("de").unwrap(), "Save"));

        // An unknown code is refused and leaves the language unchanged.
        let before = current_lang();
        assert!(!set_lang_code("xx-not-a-locale"));
        assert_eq!(current_lang(), before);

        // The macro family covers the four lookup kinds (English here, so each is its source).
        set_lang(Lang::EN);
        assert_eq!(crate::tr!("Save"), "Save");
        assert_eq!(crate::trc!("menu", "Save"), "Save");
        assert_eq!(crate::trid!("file.save", "Save"), "Save");
        assert_eq!(crate::trn!(2, "{n} item", "{n} items"), "2 items");

        // Leave the global back at English for any later reader.
        set_lang(Lang::EN);
    }
}
