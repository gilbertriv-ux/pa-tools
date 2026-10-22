//! Runtime discovery of speech model weights.
//!
//! Weights are never committed or compiled in, so a running program looks for them. A model counts
//! as found in a folder only when [`models::installed`] says all of its files are there with their
//! catalogue sizes. Folders are tried in this order and the first that has the model wins:
//!
//! 1. `$PA_SPEECH_MODELS_DIR` (command line tools and test runners);
//! 2. the application bundle's resources, `<bundle>/Contents/Resources/models` (macOS: found from
//!    the running executable at `<bundle>/Contents/MacOS/<bin>`; elsewhere that path never matches);
//! 3. the user's application data folder, where a download is stored:
//!    - macOS: `~/Library/Application Support/<product name>/models`;
//!    - Windows: `%APPDATA%\<product name>\models`;
//!    - elsewhere: `$XDG_DATA_HOME/<folder>/models`, else `~/.local/share/<folder>/models`, where
//!      `<folder>` is the product name in lower case without spaces ([`folder_slug`]).
//!
//! The crate is standalone and does not know the product's name: the caller passes it in
//! ([`ModelSearch::from_environment`]). Nothing here reads the environment except
//! `from_environment`; [`ModelSearch::from_parts`] takes the lookups as arguments so that the
//! locations can be tested without touching the process environment.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::{SpeechError, models};

/// Environment variable naming a models folder (the first place searched).
pub const MODELS_DIR_ENV: &str = "PA_SPEECH_MODELS_DIR";
/// The folder inside a search root that holds the per-model folders.
const MODELS_SUBDIR: &str = "models";

/// Which operating system's folder conventions to follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Windows,
    /// Linux, FreeBSD and the other Unixes (the XDG conventions).
    Other,
}

impl Platform {
    /// The platform this program was built for.
    pub const fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

/// The folders to look in for a model, in search order. A tier that is `None` has no location on
/// this system (unset variable, not running from a bundle, no home folder).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelSearch {
    /// `$PA_SPEECH_MODELS_DIR`.
    pub env: Option<PathBuf>,
    /// `<app bundle>/Contents/Resources/models`.
    pub bundle_resources: Option<PathBuf>,
    /// The user's application data folder for models.
    pub app_support: Option<PathBuf>,
}

impl ModelSearch {
    /// The search for the running program on this system. `product_name` is the product's display
    /// name (it names the application data folder on macOS and Windows).
    pub fn from_environment(product_name: &str) -> ModelSearch {
        // Follow symbolic links, so a command-line shim that points into a bundle still finds it.
        let exe = std::env::current_exe().ok().map(|p| p.canonicalize().unwrap_or(p));
        ModelSearch::from_parts(Platform::current(), product_name, |name| std::env::var_os(name), exe.as_deref())
    }

    /// [`from_environment`](Self::from_environment) with the environment lookup, the executable's
    /// path and the platform given. An unset or empty variable counts as absent.
    pub fn from_parts(platform: Platform, product_name: &str, env: impl Fn(&str) -> Option<OsString>, exe: Option<&Path>) -> ModelSearch {
        let var = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let app_support = match platform {
            Platform::Mac => var("HOME").map(|h| h.join("Library").join("Application Support").join(product_name).join(MODELS_SUBDIR)),
            Platform::Windows => var("APPDATA").map(|a| a.join(product_name).join(MODELS_SUBDIR)),
            // The XDG rules ignore a relative `XDG_DATA_HOME`.
            Platform::Other => var("XDG_DATA_HOME")
                .filter(|d| d.is_absolute())
                .or_else(|| var("HOME").map(|h| h.join(".local").join("share")))
                .map(|d| d.join(folder_slug(product_name)).join(MODELS_SUBDIR)),
        };
        ModelSearch { env: var(MODELS_DIR_ENV), bundle_resources: exe.and_then(bundle_resources), app_support }
    }

    /// The three tiers in search order: label, what an absent one means, and the folder.
    fn tiers(&self) -> [(&'static str, &'static str, Option<&Path>); 3] {
        [
            (MODELS_DIR_ENV, "not set", self.env.as_deref()),
            ("app bundle resources", "not running from an app bundle", self.bundle_resources.as_deref()),
            ("application data", "no home folder", self.app_support.as_deref()),
        ]
    }
}

/// The product name as a folder name for the XDG locations: lower case letters and digits only
/// (`"Pa Tools"` becomes `"patools"`).
pub fn folder_slug(product_name: &str) -> String {
    product_name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// `<bundle>/Contents/Resources/models` for an executable at `<bundle>/Contents/MacOS/<bin>`.
fn bundle_resources(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let shaped = macos.file_name() == Some(OsStr::new("MacOS")) && contents.file_name() == Some(OsStr::new("Contents"));
    shaped.then(|| contents.join("Resources").join(MODELS_SUBDIR))
}

/// The models folder that has catalogue model `model_id` installed, searching in order: the
/// environment, the bundle's resources, the application data folder. A folder that exists but
/// lacks the model (or has it half downloaded) is skipped.
///
/// Errors: [`SpeechError::UnknownModel`] for an id outside the catalogue, and
/// [`SpeechError::ModelUnavailable`] (naming every place searched) when no folder has it.
pub fn resolve_models_dir(search: &ModelSearch, model_id: &str) -> Result<PathBuf, SpeechError> {
    let model = models::find(model_id).ok_or_else(|| SpeechError::UnknownModel(model_id.into()))?;
    for (_, _, dir) in search.tiers() {
        if let Some(dir) = dir
            && models::installed(dir, model)
        {
            return Ok(dir.to_path_buf());
        }
    }
    let searched = search
        .tiers()
        .iter()
        .map(|(label, absent, dir)| match dir {
            Some(dir) => format!("{label} {}", dir.display()),
            None => format!("{label} ({absent})"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(SpeechError::ModelUnavailable(format!("speech model `{model_id}` not found (searched: {searched})")))
}
