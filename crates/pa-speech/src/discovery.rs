//! Locate a model store without depending on the launching shell's environment.
//!
//! Every returned path is a store root containing `<model id>/`, as required by [`crate::load`].

use std::path::{Path, PathBuf};

use crate::{SpeechError, models};

/// Search the environment override, an application bundle, then the user's model store.
/// `app_dir_name` comes from the host's brand configuration; this crate stays standalone.
pub fn resolve_models_dir(id: &str, app_dir_name: &str) -> Result<PathBuf, SpeechError> {
    let model = models::find(id).ok_or_else(|| SpeechError::UnknownModel(id.into()))?;
    let override_dir = std::env::var_os("PA_SPEECH_MODELS_DIR").filter(|s| !s.is_empty()).map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    let mut roots = candidates(override_dir.as_deref(), exe.as_deref());
    let user_dir = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support").join(app_dir_name))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join(app_dir_name))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|p| p.join(app_dir_name))
    };
    roots.extend(user_dir.map(|p| p.join("models")));
    first_installed(roots, model).ok_or_else(|| SpeechError::NotInstalled(id.into()))
}

fn candidates(override_dir: Option<&Path>, exe: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<_> = override_dir.into_iter().map(Path::to_path_buf).collect();
    if let Some(macos) = exe.and_then(Path::parent)
        && macos.file_name().is_some_and(|n| n == "MacOS")
        && let Some(contents) = macos.parent().filter(|p| p.file_name().is_some_and(|n| n == "Contents"))
    {
        roots.push(contents.join("Resources/models"));
    }
    roots
}

fn first_installed(roots: impl IntoIterator<Item = PathBuf>, model: &models::ModelInfo) -> Option<PathBuf> {
    roots.into_iter().find(|root| models::installed(root, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_discovery_returns_the_store_root_not_the_model_leaf() {
        let roots = candidates(Some(Path::new("/override")), Some(Path::new("/Apps/Pa Tools.app/Contents/MacOS/Pa Tools")));
        assert_eq!(roots, vec![PathBuf::from("/override"), PathBuf::from("/Apps/Pa Tools.app/Contents/Resources/models")]);
        assert_eq!(candidates(None, Some(Path::new("/tmp/debug/pa-tools"))), Vec::<PathBuf>::new());
    }

    #[test]
    fn an_incomplete_override_falls_back_without_mutating_process_environment() {
        let root = std::env::temp_dir().join(format!("pa-discovery-{}", std::process::id()));
        let model = models::find(models::DEFAULT_MODEL).unwrap();
        let fallback = root.join("Library/Application Support/Pa Tools/models");
        let leaf = fallback.join(model.id);
        std::fs::create_dir_all(&leaf).unwrap();
        for file in model.files {
            std::fs::File::create(leaf.join(file.name)).unwrap().set_len(file.size).unwrap();
        }
        assert_eq!(first_installed([root.join("missing"), fallback.clone()], model), Some(fallback.clone()));
        // All four pinned files are necessary, including generation_config.json.
        std::fs::remove_file(leaf.join("generation_config.json")).unwrap();
        assert_eq!(first_installed([fallback], model), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
