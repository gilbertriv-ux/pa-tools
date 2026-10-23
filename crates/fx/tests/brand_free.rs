//! No effect name, id, parameter label or choice label names a third party's product or company.

use soundcraft_fx::{BRAND_TERMS, brand_term_in, effects};

#[test]
fn the_matcher_finds_whole_words_only() {
    assert_eq!(brand_term_in("Pro Tools Limiter"), Some("pro tools"));
    assert_eq!(brand_term_in("Ozone-style EQ"), Some("ozone"));
    assert_eq!(brand_term_in("GuitarSuite"), Some("guitarsuite"));
    assert_eq!(brand_term_in("Enhance Speech"), Some("enhance speech"));
    assert_eq!(brand_term_in("Binauralizer - Ambisonics"), Some("binauralizer"));
    // Substrings of ordinary words are fine.
    assert_eq!(brand_term_in("Wavesetter wavelength"), None);
    assert_eq!(brand_term_in("Classic Filter"), None);
    assert_eq!(brand_term_in("Tube Compressor"), None);
    assert_eq!(brand_term_in("Master Chain"), None);
    assert!(BRAND_TERMS.iter().all(|t| *t == t.to_lowercase() && !t.contains("  ") && !t.is_empty()));
}

#[test]
fn no_registered_effect_names_a_third_party() {
    let mut checked = 0;
    for e in effects() {
        let mut texts: Vec<&str> = vec![e.id, e.name];
        for p in e.params {
            texts.push(p.id);
            texts.push(p.name);
            texts.extend(p.choices.iter().copied());
        }
        for t in texts {
            checked += 1;
            assert_eq!(brand_term_in(t), None, "{}: {t:?} names a third party", e.id);
        }
    }
    assert!(checked > 1000, "only {checked} strings checked");
}

#[test]
fn the_donor_names_are_gone() {
    for old in [
        "dehum",
        "denoise",
        "deesser",
        "dereverb",
        "speech_enhance",
        "vocal_enhancer",
        "mastering",
        "guitar_suite",
        "binauralizer",
        "ambisonics_panner",
        "scientific_filter",
        "fft_filter",
        "analog_delay",
        "stereo_expander",
    ] {
        assert!(soundcraft_fx::effect_info(old).is_none(), "{old} is still registered");
    }
    for new in [
        "hum_remover",
        "noise_reducer",
        "sibilance_reducer",
        "reverb_reducer",
        "speech_cleanup",
        "vocal_polish",
        "master_chain",
        "guitar_amp_suite",
        "binaural_panner",
        "ambisonic_rotator",
        "classic_filter",
        "spectral_curve_eq",
        "tape_delay",
        "stereo_spread",
    ] {
        assert!(soundcraft_fx::effect_info(new).is_some(), "{new} is missing");
    }
}

/// The names are neutral and readable: no run-together product style ("DeHum", "GuitarSuite"),
/// no trailing catalogue qualifiers.
#[test]
fn display_names_are_plain_words() {
    for e in effects() {
        assert!(e.name.len() >= 3 && e.name.len() <= 32, "{}: {:?}", e.id, e.name);
        assert!(!e.name.contains(" - ") && !e.name.contains('™') && !e.name.contains('®'), "{}: {:?}", e.id, e.name);
        // CamelCase run-together: a lower-case letter directly followed by an upper-case one.
        let chars: Vec<char> = e.name.chars().collect();
        assert!(!chars.windows(2).any(|w| w[0].is_lowercase() && w[1].is_uppercase()), "{}: {:?} looks like a product style", e.id, e.name);
    }
}
