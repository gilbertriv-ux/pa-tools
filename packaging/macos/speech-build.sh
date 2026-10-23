#!/usr/bin/env bash
# Local speech-enabled app: speech-build.sh <new-output-directory>
# All downloads, fixtures and app files stay under that new directory; no existing build is replaced.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUN_DIR="${1:?usage: speech-build.sh <new-output-directory>}"
if [ "$(uname -s)" != Darwin ]; then
  echo "This runner requires macOS (say, afconvert and codesign)." >&2
  exit 2
fi
mkdir "$RUN_DIR"
RUN_DIR="$(cd "$RUN_DIR" && pwd)"
exec > >(tee "$RUN_DIR/verification.log") 2>&1
cd "$ROOT"
mkdir "$RUN_DIR/models" "$RUN_DIR/fixtures"
export PA_SPEECH_MODELS_DIR="$RUN_DIR/models"
export PA_SPEECH_FIXTURE="$RUN_DIR/fixtures/speech.f32"
export PA_SPEECH_EXPECTED_KEYWORD=transcription
export PA_SPEECH_WAV_FIXTURE="$RUN_DIR/fixtures/speech.wav"
export SOUNDCRAFT_NO_PREFS=1

# The catalogue pins four files, including generation_config.json; cached files are hashed too.
cargo run --locked -p soundcraft-pa-speech --features download --example download_model -- "$PA_SPEECH_MODELS_DIR"
curl --fail --location --silent --show-error https://raw.githubusercontent.com/openai/whisper/main/LICENSE \
  --output "$PA_SPEECH_MODELS_DIR/WHISPER-LICENSE"
say --file-format=caff --data-format=LEF32@16000 -o "$RUN_DIR/fixtures/speech.caf" \
  "Testing speech transcription for our audio workstation. The quick brown fox jumps over the lazy dog."
afconvert -f WAVE -d LEI16@16000 -c 1 "$RUN_DIR/fixtures/speech.caf" "$RUN_DIR/fixtures/speech.wav"
python3 - "$RUN_DIR/fixtures/speech.wav" "$PA_SPEECH_FIXTURE" <<'PY'
import struct, sys, wave
with wave.open(sys.argv[1], 'rb') as wav:
    assert (wav.getnchannels(), wav.getframerate(), wav.getsampwidth()) == (1, 16000, 2)
    raw = wav.readframes(wav.getnframes())
samples = struct.unpack('<' + 'h' * (len(raw) // 2), raw)
with open(sys.argv[2], 'xb') as out:
    out.write(struct.pack('<' + 'f' * len(samples), *[s / 32768.0 for s in samples]))
PY

cargo fmt --all -- --check
cargo check --locked -p soundcraft -p soundcraft-cli --features whisper,download
cargo clippy --locked -p soundcraft-pa-speech -p soundcraft-engine -p soundcraft-ui-egui --all-targets --features whisper,download -- -D warnings
cargo test --locked -p soundcraft-pa-speech --features whisper,download -- --nocapture
cargo test --locked -p soundcraft-engine --test speech_model --features whisper,download -- --nocapture
cargo test --locked -p soundcraft-ui-egui --test transcript_window --features whisper,download
cargo xtask layers
RELEASE_TARGET="${SPEECH_RELEASE_TARGET_DIR:-${CARGO_TARGET_DIR:-$ROOT/target}}"
MACOSX_DEPLOYMENT_TARGET=11.0 CARGO_TARGET_DIR="$RELEASE_TARGET" cargo build --release --locked -p soundcraft -p soundcraft-cli --features whisper,download

# Cargo metadata gives the actual target directory and package version; no hard-coded release number.
CARGO_TARGET_DIR="$RELEASE_TARGET" cargo metadata --no-deps --format-version 1 > "$RUN_DIR/metadata.json"
python3 - "$ROOT" "$RUN_DIR" <<'PY'
import json, pathlib, plistlib, shutil, subprocess, sys
root, run = map(pathlib.Path, sys.argv[1:])
metadata = json.loads((run / 'metadata.json').read_text())
package = next(p for p in metadata['packages'] if p['name'] == 'soundcraft')
version = package['version']
app = run / 'Pa Tools.app'
macos, resources = app / 'Contents/MacOS', app / 'Contents/Resources'
macos.mkdir(parents=True)
resources.mkdir()
for name in ['LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'ATTRIBUTION.md']:
    shutil.copy2(root / name, resources / name)
target = pathlib.Path(metadata['target_directory'])
shutil.copy2(target / 'release/pa-tools', macos / 'Pa Tools')
shutil.copy2(target / 'release/pa-tools-cli', run / 'pa-tools-cli')
shutil.copy2(root / 'assets/app-icon/Pa Tools.icns', resources / 'Pa Tools.icns')
shutil.copytree(run / 'models', resources / 'models')
template = (root / 'packaging/macos/Info.plist.in').read_text()
sha = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
template = template.replace('@VERSION@', version).replace('@SHORT_VERSION@', version.split('-')[0]).replace('@BUILD_SHA@', sha)
with (app / 'Contents/Info.plist').open('xb') as out:
    out.write(template.encode())
with (app / 'Contents/Info.plist').open('rb') as source:
    plistlib.load(source)
PY
APP="$RUN_DIR/Pa Tools.app"
codesign --force --sign - --options runtime --timestamp=none --entitlements "$ROOT/packaging/macos/entitlements.plist" "$APP/Contents/MacOS/Pa Tools"
codesign --force --sign - --options runtime --timestamp=none --entitlements "$ROOT/packaging/macos/entitlements.plist" "$APP"
codesign --verify --strict --deep "$APP"
"$APP/Contents/MacOS/Pa Tools" --version
echo "Verified local app: $APP"
