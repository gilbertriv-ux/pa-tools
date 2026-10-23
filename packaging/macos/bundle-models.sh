#!/usr/bin/env bash
# Copies a speech models folder into an app bundle. Sourced by package.sh (which has `warn`), and
# exercised by bundle-models.test.sh.
#
#   bundle_speech_models <models folder> <bundle Resources folder>
#
# Only visible entries of the models folder are copied (a lone .DS_Store is not a model), and
# symbolic links are followed (`cp -RL`) so the bundle is self-contained: a link into the build
# machine's folders would be a dangling file on every other Mac. Quote everything: both paths may
# contain spaces. Never fails the build by itself: a missing, empty or incomplete models folder is a
# warning and the build carries on without (or with a partial) model.

bundle_speech_models() {
  local src="$1" resources="$2" dest item copied=0 weights=0 w
  dest="$resources/models"
  if [ -d "$src" ]; then
    for item in "$src"/*; do
      [ -e "$item" ] || continue # the glob matched nothing: the folder is empty or holds only hidden files
      mkdir -p "$dest"
      cp -RL "$item" "$dest/"
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
