//! Model discovery: where the speech model is looked for, in what order, and what happens when it
//! is nowhere. Always runs (no model weights, no feature): fake installs are sparse files with the
//! catalogue's exact sizes, which is all `models::installed` checks. The process environment is
//! never changed; locations come from `ModelSearch::from_parts` with injected lookups.

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use soundcraft_pa_speech::{ModelSearch, Platform, SpeechError, models, resolve_models_dir};

const MODEL: &str = "whisper-base";

/// A scratch folder removed when dropped, so a failing assertion still cleans up.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("pa-speech-discovery-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch folder");
        Scratch(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Install `model_id` under `models_dir/<id>/` as sparse files of the catalogue sizes.
fn fake_install(models_dir: &Path, model_id: &str) {
    let m = models::find(model_id).expect("model in the catalogue");
    let dir = models::model_dir(models_dir, m);
    std::fs::create_dir_all(&dir).expect("create model folder");
    for f in m.files {
        File::create(dir.join(f.name)).and_then(|file| file.set_len(f.size)).expect("create sized file");
    }
    assert!(models::installed(models_dir, m), "the fake install must satisfy `installed`");
}

#[test]
fn falls_back_to_the_application_support_folder() {
    // The V5.5 fallback: no environment folder, no bundle, the model in Application Support.
    let tmp = Scratch::new("appsupport");
    let models_dir = tmp.path().join("Application Support").join("Pa Tools").join("models");
    fake_install(&models_dir, MODEL);
    let search = ModelSearch { env: None, bundle_resources: None, app_support: Some(models_dir.clone()) };
    assert_eq!(resolve_models_dir(&search, MODEL), Ok(models_dir.clone()));
    assert!(models_dir.join(MODEL).join("model.safetensors").is_file());
}

#[test]
fn order_is_environment_then_bundle_then_application_support() {
    let tmp = Scratch::new("order");
    let (env, bundle, support) = (tmp.path().join("env"), tmp.path().join("bundle"), tmp.path().join("support"));
    for d in [&env, &bundle, &support] {
        fake_install(d, MODEL);
    }
    let all = ModelSearch { env: Some(env.clone()), bundle_resources: Some(bundle.clone()), app_support: Some(support.clone()) };
    assert_eq!(resolve_models_dir(&all, MODEL), Ok(env.clone()));

    let no_env = ModelSearch { env: None, ..all.clone() };
    assert_eq!(resolve_models_dir(&no_env, MODEL), Ok(bundle.clone()));

    let only_support = ModelSearch { env: None, bundle_resources: None, ..all.clone() };
    assert_eq!(resolve_models_dir(&only_support, MODEL), Ok(support));
}

#[test]
fn a_folder_without_the_model_is_skipped() {
    let tmp = Scratch::new("skip");
    let (empty_env, bundle_other, support) = (tmp.path().join("env"), tmp.path().join("bundle"), tmp.path().join("support"));
    std::fs::create_dir_all(&empty_env).expect("create env folder"); // exists, holds nothing
    fake_install(&bundle_other, "whisper-tiny"); // a different model
    // Half a download: right files, one of them the wrong size.
    fake_install(&support, MODEL);
    let m = models::find(MODEL).expect("model in the catalogue");
    let weights = models::model_dir(&support, m).join("model.safetensors");
    File::options().write(true).open(&weights).and_then(|f| f.set_len(1)).expect("truncate weights");
    let bundle = tmp.path().join("bundle2");
    fake_install(&bundle, MODEL);

    // The first three tiers lack `whisper-base` (empty / other model / truncated): the last good one wins.
    let search = ModelSearch { env: Some(empty_env.clone()), bundle_resources: Some(bundle_other.clone()), app_support: Some(support.clone()) };
    assert!(matches!(resolve_models_dir(&search, MODEL), Err(SpeechError::ModelUnavailable { .. })));
    let with_good_bundle = ModelSearch { bundle_resources: Some(bundle.clone()), ..search.clone() };
    assert_eq!(resolve_models_dir(&with_good_bundle, MODEL), Ok(bundle));
    // The other model is found where it is.
    assert_eq!(resolve_models_dir(&search, "whisper-tiny"), Ok(bundle_other));
}

#[test]
fn nothing_installed_is_model_unavailable_naming_every_place_searched() {
    let tmp = Scratch::new("none");
    let search =
        ModelSearch { env: Some(tmp.path().join("env-models")), bundle_resources: None, app_support: Some(tmp.path().join("support-models")) };
    let err = resolve_models_dir(&search, MODEL).expect_err("no model anywhere");
    let SpeechError::ModelUnavailable { model, searched: text } = &err else { panic!("expected ModelUnavailable, got {err:?}") };
    assert_eq!(model, MODEL);
    // The message names the model by its display name, never by its id.
    assert!(err.to_string().starts_with("Speech model, base (multilingual) not found (searched: "), "{err}");
    assert!(!err.to_string().to_lowercase().contains("whisper"), "{err}");
    assert!(text.contains("PA_SPEECH_MODELS_DIR") && text.contains("env-models"), "{text}");
    assert!(text.contains("app bundle resources (not running from an app bundle)"), "{text}");
    assert!(text.contains("application data") && text.contains("support-models"), "{text}");
    assert!(err.to_string().ends_with(&format!("(searched: {text})")), "the error prints the places searched");

    // No tier at all is the same error, never a panic or an empty Ok.
    assert!(matches!(resolve_models_dir(&ModelSearch::default(), MODEL), Err(SpeechError::ModelUnavailable { .. })));
    // An id outside the catalogue is a different error.
    assert_eq!(resolve_models_dir(&search, "whisper-nope"), Err(SpeechError::UnknownModel("whisper-nope".into())));
}

/// An environment made of `pairs`, for `ModelSearch::from_parts`.
fn env_of(pairs: &[(&'static str, String)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let pairs: Vec<(&'static str, String)> = pairs.to_vec();
    move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| OsString::from(v))
}

/// A path built the way the code under test builds one (`PathBuf::join`), so that an expectation
/// holds on every host. The simulated platforms differ in *which* folders they use; the separator
/// and the notion of "absolute" are the host's, because that is what `PathBuf` is. The exact text of
/// a path as spelled on this host is pinned by the `#[cfg]` tests below.
fn joined(base: &str, parts: &[&str]) -> PathBuf {
    parts.iter().fold(PathBuf::from(base), |path, part| path.join(part))
}

/// A folder that is absolute on this host: `/data` has no root on Windows (it needs a drive), so
/// a test of "an absolute `XDG_DATA_HOME` is honoured" must not hard-code the Unix spelling.
fn absolute(name: &str) -> String {
    if cfg!(windows) { format!("C:\\{name}") } else { format!("/{name}") }
}

#[test]
fn locations_follow_each_platforms_conventions() {
    let none: Option<&Path> = None;

    let mac = ModelSearch::from_parts(Platform::Mac, "Pa Tools", "patools", env_of(&[("HOME", "/Users/user".into())]), none);
    assert_eq!(mac.app_support, Some(joined("/Users/user", &["Library", "Application Support", "Pa Tools", "models"])));
    assert_eq!((mac.env, mac.bundle_resources), (None, None));

    // Windows: the local (non-roaming) folder first, the roaming one only as the fallback.
    let both = [("LOCALAPPDATA", "C:\\Users\\user\\AppData\\Local".to_string()), ("APPDATA", "C:\\Users\\user\\AppData\\Roaming".to_string())];
    let win = ModelSearch::from_parts(Platform::Windows, "Pa Tools", "patools", env_of(&both), none);
    assert_eq!(win.app_support, Some(joined("C:\\Users\\user\\AppData\\Local", &["Pa Tools", "models"])));
    let roaming_only = ModelSearch::from_parts(Platform::Windows, "Pa Tools", "patools", env_of(&both[1..]), none);
    assert_eq!(roaming_only.app_support, Some(joined("C:\\Users\\user\\AppData\\Roaming", &["Pa Tools", "models"])));
    let empty_local = [("LOCALAPPDATA", String::new()), both[1].clone()];
    let ignores_empty = ModelSearch::from_parts(Platform::Windows, "Pa Tools", "patools", env_of(&empty_local), none);
    assert_eq!(ignores_empty.app_support, roaming_only.app_support, "an empty LOCALAPPDATA counts as unset");

    // Elsewhere: an absolute XDG_DATA_HOME, else ~/.local/share.
    let data = absolute("data");
    let xdg = ModelSearch::from_parts(
        Platform::Other,
        "Pa Tools",
        "patools",
        env_of(&[("XDG_DATA_HOME", data.clone()), ("HOME", "/home/user".into())]),
        none,
    );
    assert_eq!(xdg.app_support, Some(joined(&data, &["patools", "models"])));
    let home_share = joined("/home/user", &[".local", "share", "patools", "models"]);
    let home = ModelSearch::from_parts(Platform::Other, "Pa Tools", "patools", env_of(&[("HOME", "/home/user".into())]), none);
    assert_eq!(home.app_support, Some(home_share.clone()));
    // A relative or empty XDG_DATA_HOME is ignored (as the XDG rules say).
    for bad in ["relative/dir", ""] {
        let s = ModelSearch::from_parts(
            Platform::Other,
            "Pa Tools",
            "patools",
            env_of(&[("XDG_DATA_HOME", bad.into()), ("HOME", "/home/user".into())]),
            none,
        );
        assert_eq!(s.app_support, Some(home_share.clone()), "XDG_DATA_HOME={bad:?}");
    }

    // No home folder, no location: the tier is absent, not a guess.
    for platform in [Platform::Mac, Platform::Windows, Platform::Other] {
        assert_eq!(ModelSearch::from_parts(platform, "Pa Tools", "patools", env_of(&[]), none), ModelSearch::default(), "{platform:?}");
    }
    // The Linux folder is the name the caller passes, not something derived from the product name.
    let named = ModelSearch::from_parts(Platform::Other, "Pa Tools", "some-other-name", env_of(&[("HOME", "/home/user".into())]), none);
    assert_eq!(named.app_support, Some(joined("/home/user", &[".local", "share", "some-other-name", "models"])));
}

/// The text of a path is the host's: on Windows the real folder is `%LOCALAPPDATA%\Pa Tools\models`
/// (drive, backslashes), and the program's own search on this machine reads the Windows variables.
/// `PathBuf` equality compares components, so these compare the spelled-out text, which is what the
/// user sees in the "searched" list of the error.
#[cfg(windows)]
#[test]
fn on_windows_the_models_folder_is_a_backslash_path_under_the_local_app_data_folder() {
    let both = [("LOCALAPPDATA", r"C:\Users\user\AppData\Local".to_string()), ("APPDATA", r"C:\Users\user\AppData\Roaming".to_string())];
    let win = ModelSearch::from_parts(Platform::Windows, "Pa Tools", "patools", env_of(&both), None);
    assert_eq!(win.app_support.as_deref().and_then(Path::to_str), Some(r"C:\Users\user\AppData\Local\Pa Tools\models"));
    let roaming = ModelSearch::from_parts(Platform::Windows, "Pa Tools", "patools", env_of(&both[1..]), None);
    assert_eq!(roaming.app_support.as_deref().and_then(Path::to_str), Some(r"C:\Users\user\AppData\Roaming\Pa Tools\models"));

    // The search for this machine: the platform is Windows, and it reads this machine's variables.
    assert_eq!(Platform::current(), Platform::Windows);
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let expected = var("LOCALAPPDATA").or_else(|| var("APPDATA")).map(|base| PathBuf::from(base).join("Pa Tools").join("models"));
    let real = ModelSearch::from_environment("Pa Tools", "patools");
    assert_eq!(real.app_support, expected);
    if let Some(text) = real.app_support.as_deref().and_then(Path::to_str) {
        assert!(text.ends_with(r"\Pa Tools\models"), "{text}");
    }
}

/// Everywhere else the folders are spelled with forward slashes (compared as text, not components).
#[cfg(not(windows))]
#[test]
fn off_windows_the_models_folders_are_forward_slash_paths() {
    let text = |s: ModelSearch| s.app_support.as_deref().and_then(Path::to_str).map(str::to_string);
    let mac = ModelSearch::from_parts(Platform::Mac, "Pa Tools", "patools", env_of(&[("HOME", "/Users/user".into())]), None);
    assert_eq!(text(mac).as_deref(), Some("/Users/user/Library/Application Support/Pa Tools/models"));
    let xdg = ModelSearch::from_parts(Platform::Other, "Pa Tools", "patools", env_of(&[("XDG_DATA_HOME", "/data".into())]), None);
    assert_eq!(text(xdg).as_deref(), Some("/data/patools/models"));
    let home = ModelSearch::from_parts(Platform::Other, "Pa Tools", "patools", env_of(&[("HOME", "/home/user".into())]), None);
    assert_eq!(text(home).as_deref(), Some("/home/user/.local/share/patools/models"));
}

#[test]
fn environment_tier_comes_from_pa_speech_models_dir_and_ignores_empty() {
    let set = ModelSearch::from_parts(Platform::Other, "Pa Tools", "patools", env_of(&[("PA_SPEECH_MODELS_DIR", "/models".into())]), None);
    assert_eq!(set.env, Some(PathBuf::from("/models")));
    let empty = ModelSearch::from_parts(Platform::Other, "Pa Tools", "patools", env_of(&[("PA_SPEECH_MODELS_DIR", String::new())]), None);
    assert_eq!(empty.env, None);
}

#[test]
fn bundle_resources_are_found_from_the_executable_inside_a_bundle() {
    let exe = Path::new("/Applications/Pa Tools.app/Contents/MacOS/pa-tools");
    let s = ModelSearch::from_parts(Platform::Mac, "Pa Tools", "patools", env_of(&[]), Some(exe));
    assert_eq!(s.bundle_resources, Some(PathBuf::from("/Applications/Pa Tools.app/Contents/Resources/models")));

    // Not in a bundle (a development build, a Linux install, a bare path): no bundle tier.
    for outside in ["/usr/local/bin/pa-tools", "target/debug/pa-tools", "/opt/pa-tools/MacOS/pa-tools", "pa-tools", "/"] {
        let s = ModelSearch::from_parts(Platform::Other, "Pa Tools", "patools", env_of(&[]), Some(Path::new(outside)));
        assert_eq!(s.bundle_resources, None, "{outside}");
    }
}

#[test]
fn a_macos_style_install_resolves_end_to_end() {
    // Executable in a bundle, model shipped in its Resources: found without any environment.
    let tmp = Scratch::new("bundle");
    let exe = tmp.path().join("Pa Tools.app").join("Contents").join("MacOS").join("pa-tools");
    let search = ModelSearch::from_parts(
        Platform::Mac,
        "Pa Tools",
        "patools",
        env_of(&[("HOME", tmp.path().join("home").to_string_lossy().into_owned())]),
        Some(&exe),
    );
    let resources = tmp.path().join("Pa Tools.app").join("Contents").join("Resources").join("models");
    assert_eq!(search.bundle_resources.as_deref(), Some(resources.as_path()));
    assert!(matches!(resolve_models_dir(&search, MODEL), Err(SpeechError::ModelUnavailable { .. })), "nothing installed yet");

    // A download in Application Support is found when the bundle has nothing...
    let support = tmp.path().join("home").join("Library").join("Application Support").join("Pa Tools").join("models");
    assert_eq!(search.app_support.as_deref(), Some(support.as_path()));
    fake_install(&support, MODEL);
    assert_eq!(resolve_models_dir(&search, MODEL), Ok(support));

    // ...and the bundled copy wins once it is there.
    fake_install(&resources, MODEL);
    assert_eq!(resolve_models_dir(&search, MODEL), Ok(resources));
}
