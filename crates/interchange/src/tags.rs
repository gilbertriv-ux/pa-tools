//! Names of the private tagged values (AAF) and properties (OMF) this crate writes, and the older
//! names it still reads.
//!
//! The names are machine-read metadata, never shown by this application; other applications list
//! them as plain tags. The prefix comes from the brand crate so a rebrand changes it in one place.

use soundcraft_brand::INTERCHANGE_PREFIX;

/// Tag suffixes (AAF: `"<prefix> <suffix>"`).
pub(crate) const FRAME_SIZE: &str = "Frame Size";
pub(crate) const AUDIO_SAMPLE_RATE: &str = "Audio Sample Rate";
pub(crate) const EFFECT: &str = "Effect";

/// The prefix of files written by the project this code was ported from. Read-only: such files
/// import with their tags understood, nothing is ever written with it.
const LEGACY_PREFIX: &str = "FilmCraft";

/// The AAF tagged-value name `"<prefix> <suffix>"` written by this crate.
pub(crate) fn aaf(suffix: &str) -> String {
    format!("{INTERCHANGE_PREFIX} {suffix}")
}

/// Whether the tagged-value name `found` is `suffix` under the current or the legacy prefix.
pub(crate) fn aaf_is(found: &str, suffix: &str) -> bool {
    found.strip_suffix(suffix).and_then(|p| p.strip_suffix(' ')).is_some_and(|p| p == INTERCHANGE_PREFIX || p == LEGACY_PREFIX)
}

/// The OMF property name `"<prefix>:<class>:<field>"` written by this crate.
pub(crate) fn omf(class: &str, field: &str) -> String {
    format!("{INTERCHANGE_PREFIX}:{class}:{field}")
}

/// The same property under the legacy prefix (for reading).
pub(crate) fn omf_legacy(class: &str, field: &str) -> String {
    format!("{LEGACY_PREFIX}:{class}:{field}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_branded_and_legacy_names_are_read() {
        assert_eq!(aaf(FRAME_SIZE), "PaTools Frame Size");
        assert!(aaf_is("PaTools Effect", EFFECT));
        assert!(aaf_is("FilmCraft Effect", EFFECT));
        assert!(!aaf_is("Other Effect", EFFECT));
        assert!(!aaf_is("PaToolsEffect", EFFECT));
        assert!(!aaf_is("Effect", EFFECT));
        assert_eq!(omf("SCLP", "ClipName"), "PaTools:SCLP:ClipName");
        assert_eq!(omf_legacy("SCLP", "ClipName"), "FilmCraft:SCLP:ClipName");
    }
}
