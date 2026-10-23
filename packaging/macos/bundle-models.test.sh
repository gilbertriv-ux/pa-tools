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
[ "$(cat "$res/models/linked-model/model.safetensors")" = realweights ] || fail "the linked folder was copied empty"
[ -z "$(find "$res/models" -type l)" ] || fail "the bundle holds symbolic links"
echo "ok  symbolic links are followed (self-contained bundle)"

# 5. Folders without a model.safetensors: copied, but warned about.
fresh; src="$scratch/no weights"; mkdir -p "$src/whisper-base"; echo '{}' >"$src/whisper-base/config.json"
bundle_speech_models "$src" "$res" >/dev/null
[ -f "$res/models/whisper-base/config.json" ] || fail "partial model not copied"
case "$warnings" in *"model.safetensors"*) ;; *) fail "no warning for a model without weights: $warnings" ;; esac
echo "ok  a model without model.safetensors warns"
# 6. A dangling link inside a model folder (the weights were moved away): the build is not aborted
#    (set -e is on), the rest of the folder is copied, nothing dangling lands in the bundle, and the
#    warning names the link. With no weights left, the incomplete-model warning follows too.
fresh; src="$scratch/dangling inside"; mkdir -p "$src/whisper-base"
echo '{}' >"$src/whisper-base/config.json"; echo tok >"$src/whisper-base/tokenizer.json"
ln -s "$scratch/nowhere at all/model.safetensors" "$src/whisper-base/model.safetensors"
bundle_speech_models "$src" "$res" >/dev/null || fail "a dangling link made bundle_speech_models fail"
[ -f "$res/models/whisper-base/config.json" ] && [ -f "$res/models/whisper-base/tokenizer.json" ] || fail "the other files were not copied"
[ ! -e "$res/models/whisper-base/model.safetensors" ] && [ ! -L "$res/models/whisper-base/model.safetensors" ] || fail "the dangling link was copied"
case "$warnings" in *"model.safetensors: a symbolic link to nowhere"*) ;; *) fail "no warning naming the dangling link: $warnings" ;; esac
case "$warnings" in *"incomplete"*) ;; *) fail "no incomplete-model warning: $warnings" ;; esac
echo "ok  a dangling link inside a model folder is skipped with a warning"

# 7. The same under the real `set -e` of a caller (package.sh): the function returns 0 and the script goes on.
out="$(bash -c 'set -euo pipefail; warn() { echo "WARN: $*"; }; . "$1"; bundle_speech_models "$2" "$3"; echo reached-the-end' _ \
  "$HERE/bundle-models.sh" "$src" "$scratch/second bundle")" || fail "a caller with set -e was aborted by a dangling link"
case "$out" in *reached-the-end*) ;; *) fail "the caller did not reach its next line: $out" ;; esac
echo "ok  a caller running under set -e carries on past a dangling link"

# 8. A good model next to a dangling top-level link: the model is bundled, the link is reported.
fresh; src="$scratch/dangling beside"; mkdir -p "$src/whisper-base"
echo w >"$src/whisper-base/model.safetensors"; echo '{}' >"$src/whisper-base/config.json"
ln -s "$scratch/gone/other-model" "$src/other-model"
bundle_speech_models "$src" "$res" >/dev/null || fail "a dangling top-level link made bundle_speech_models fail"
[ -f "$res/models/whisper-base/model.safetensors" ] || fail "the good model was not copied"
[ ! -e "$res/models/other-model" ] && [ ! -L "$res/models/other-model" ] || fail "the dangling top-level link was copied"
case "$warnings" in *"other-model: a symbolic link to nowhere"*) ;; *) fail "no warning for the dangling top-level link: $warnings" ;; esac
case "$warnings" in *"incomplete"*) fail "a complete model was called incomplete: $warnings" ;; esac
echo "ok  a dangling top-level link is reported and the good model is bundled"

# 9. A link back to a folder it is inside (a loop) is skipped, not followed forever; a link to a
#    folder elsewhere is still copied as a real folder.
fresh; src="$scratch/loop models"; mkdir -p "$src/whisper-base" "$scratch/elsewhere two"
echo w >"$src/whisper-base/model.safetensors"; echo extra >"$scratch/elsewhere two/extra.txt"
ln -s .. "$src/whisper-base/up"
ln -s "$scratch/elsewhere two" "$src/whisper-base/side"
bundle_speech_models "$src" "$res" >/dev/null || fail "a link loop made bundle_speech_models fail"
[ -f "$res/models/whisper-base/model.safetensors" ] || fail "the model was not copied"
[ -f "$res/models/whisper-base/side/extra.txt" ] && [ ! -L "$res/models/whisper-base/side" ] || fail "the link to another folder was not followed"
[ ! -e "$res/models/whisper-base/up" ] || fail "the loop was copied"
case "$warnings" in *"a link back to a folder it is inside"*) ;; *) fail "no warning for the loop: $warnings" ;; esac
[ -z "$(find "$res/models" -type l)" ] || fail "the bundle holds symbolic links"
echo "ok  a link loop is skipped with a warning; a link to another folder is followed"
echo "all ok"
