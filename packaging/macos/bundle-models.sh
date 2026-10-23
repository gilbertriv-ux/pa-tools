#!/usr/bin/env bash
# Copies a speech models folder into an app bundle. Sourced by package.sh (which has `warn`), and
# exercised by bundle-models.test.sh.
#
#   bundle_speech_models <models folder> <bundle Resources folder>
#
# Only visible entries of the models folder are copied (a lone .DS_Store is not a model), and
# symbolic links are followed so the bundle is self-contained: a link into the build machine's
# folders would be a dangling file on every other Mac. A link that leads nowhere (a model folder
# whose weights were moved or deleted, a link loop) is skipped with a warning, not copied and not a
# reason to stop: `cp -RL` would abort on it. Quote everything: both paths may contain spaces. Never
# fails the build by itself: a missing, empty or incomplete models folder is a warning and the build
# carries on without (or with a partial) model.

# bundle_copy_followed <file, folder or link> <folder to copy it into> [<resolved folders above it>]
# Copies one entry into the folder, following links. Never returns non-zero for a bad link.
bundle_copy_followed() {
  local from="$1" into="$2" chain="${3:-}" name entry real
  name="$(basename "$from")"
  if [ -d "$from" ]; then # -d follows links
    real="$(cd "$from" && pwd -P)" || { warn "macOS: skipped $from: the folder cannot be opened"; return 0; }
    case ":$chain:" in
      *":$real:"*) warn "macOS: skipped $from: a link back to a folder it is inside"; return 0 ;;
    esac
    mkdir -p "$into/$name"
    while IFS= read -r -d '' entry; do
      bundle_copy_followed "$entry" "$into/$name" "$chain:$real"
    done < <(find -H "$from" -mindepth 1 -maxdepth 1 -print0) # -H: look inside a link to a folder
  elif [ -f "$from" ]; then
    cp -L "$from" "$into/$name" || warn "macOS: could not copy $from"
  elif [ -L "$from" ]; then
    warn "macOS: skipped $from: a symbolic link to nowhere"
  else
    warn "macOS: skipped $from: not a file or folder"
  fi
  return 0
}

bundle_speech_models() {
  local src="$1" resources="$2" dest item copied=0 weights=0 w here=""
  dest="$resources/models"
  if [ -d "$src" ]; then
    here="$(cd "$src" && pwd -P)" || here="" # the folders a link must not lead back to start here
    for item in "$src"/*; do
      if [ ! -e "$item" ]; then
        # The glob matched nothing (an empty folder, or only hidden files), or the entry is a link to nowhere.
        if [ -L "$item" ]; then warn "macOS: skipped $item: a symbolic link to nowhere"; fi
        continue
      fi
      mkdir -p "$dest"
      bundle_copy_followed "$item" "$dest" "$here"
      copied=1
    done
  fi
  if [ "$copied" = 0 ]; then
    warn "macOS: PT_MODELS_DIR=$src is missing or has no visible entries; no speech model bundled"
    return 0
  fi
  echo "==> bundled speech models from $src:"
  ls -l "$dest"
  for w in "$dest"/*/model.safetensors; do
    if [ -f "$w" ]; then weights=1; fi
  done
  if [ "$weights" = 0 ]; then
    warn "macOS: no <model>/model.safetensors under $dest; the bundled speech model is incomplete and the app cannot use it"
  fi
  return 0
}
