#!/usr/bin/env bash
# Exercises bundle_speech_models (bundle-models.sh) on scratch folders whose names contain spaces.
# Run: packaging/macos/bundle-models.test.sh   (prints "ok" lines; exits non-zero on the first failure)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
warnings=""
warn() { warnings="$warnings$*"$'\n'; }
# shellcheck source=bundle-models.sh
. "$HERE/bundle-models.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
scratch="$(mktemp -d "${TMPDIR:-/tmp}/bundle models test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

fresh() { warnings=""; rm -rf "$scratch/Pa Tools.app"; mkdir -p "$scratch/Pa Tools.app/Contents/Resources"; }
res="$scratch/Pa Tools.app/Contents/Resources"

# 1. A normal folder, spaces in every path: copied, no warning.
fresh; src="$scratch/my models"; mkdir -p "$src/whisper-base"
echo weights >"$src/whisper-base/model.safetensors"; echo '{}' >"$src/whisper-base/config.json"
bundle_speech_models "$src" "$res" >/dev/null
[ -f "$res/models/whisper-base/model.safetensors" ] || fail "weights not copied"
[ -z "$warnings" ] || fail "unexpected warning: $warnings"
echo "ok  copies a model folder (paths with spaces)"

# 2. Only hidden files: must not abort the script (set -e), must warn, must bundle nothing.
fresh; src="$scratch/only hidden"; mkdir -p "$src"; : >"$src/.DS_Store"; : >"$src/.hidden dir"
bundle_speech_models "$src" "$res" >/dev/null
[ ! -e "$res/models" ] || fail "something was bundled from a folder with only hidden files"
case "$warnings" in *"no visible entries"*) ;; *) fail "no warning for hidden-only folder: $warnings" ;; esac
echo "ok  a folder with only hidden files warns and does not abort"

# 3. Missing and empty folders.
fresh; bundle_speech_models "$scratch/does not exist" "$res" >/dev/null
case "$warnings" in *"no visible entries"*) ;; *) fail "no warning for a missing folder" ;; esac
fresh; mkdir -p "$scratch/empty dir"; bundle_speech_models "$scratch/empty dir" "$res" >/dev/null
case "$warnings" in *"no visible entries"*) ;; *) fail "no warning for an empty folder" ;; esac
echo "ok  missing and empty folders warn"

# 4. Symbolic links are dereferenced: the bundle holds real files, not links.
fresh; src="$scratch/linked models"; mkdir -p "$src/whisper-base" "$scratch/elsewhere dir"
echo realweights >"$scratch/elsewhere dir/model.safetensors"
ln -s "$scratch/elsewhere dir/model.safetensors" "$src/whisper-base/model.safetensors"
ln -s "$scratch/elsewhere dir" "$src/linked-model"
bundle_speech_models "$src" "$res" >/dev/null
[ -f "$res/models/whisper-base/model.safetensors" ] && [ ! -L "$res/models/whisper-base/model.safetensors" ] || fail "weights link not dereferenced"
[ -d "$res/models/linked-model" ] && [ ! -L "$res/models/linked-model" ] || fail "folder link not dereferenced"
[ "$(cat "$res/models/whisper-base/model.safetensors")" = realweights ] || fail "wrong content"
[ -z "$(find "$res/models" -type l)" ] || fail "the bundle holds symbolic links"
echo "ok  symbolic links are followed (self-contained bundle)"

# 5. Folders without a model.safetensors: copied, but warned about.
fresh; src="$scratch/no weights"; mkdir -p "$src/whisper-base"; echo '{}' >"$src/whisper-base/config.json"
bundle_speech_models "$src" "$res" >/dev/null
[ -f "$res/models/whisper-base/config.json" ] || fail "partial model not copied"
case "$warnings" in *"model.safetensors"*) ;; *) fail "no warning for a model without weights: $warnings" ;; esac
echo "ok  a model without model.safetensors warns"
echo "all ok"
