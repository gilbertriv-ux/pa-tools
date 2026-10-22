#!/usr/bin/env bash
# Regenerate every Pa Tools app icon from assets/app-icon/icon.svg.
#
# Needs: resvg (brew install resvg / cargo install resvg) and python3. The outputs are committed,
# so builds and packaging never need these tools.
#
#   packaging/icons.sh
#
# Outputs, in assets/app-icon/: patools-1024.png, patools-macos-512.png, patools.ico,
# "Pa Tools.iconset/" (the ten PNGs iconutil wants), "Pa Tools.icns", and hicolor/<n>x<n>/apps/
# com.imagescouture.patools.png plus the scalable SVG.
#
# The .icns comes from macOS' iconutil when it exists, otherwise from packaging/icons.py (PNG
# entries only; load the same, but run this on a Mac once for the canonical file, or just
#   iconutil -c icns "assets/app-icon/Pa Tools.iconset"
# ). The .ico comes from packaging/icons.py; ICO_PACKER=xtask uses `cargo xtask ico` instead
# (same layout).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/app-icon"
SVG="$DIR/icon.svg"
# >>> brand (rebrand.sh keeps these in sync with its table)
ID="com.imagescouture.patools"
NAME="Pa Tools"
SLUG="patools"
# <<< brand
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

command -v resvg >/dev/null || { echo "error: resvg not found (brew install resvg)" >&2; exit 1; }

# The artwork is a full-bleed 512 tile (rx=112). Windows and Linux use it as is. macOS icons sit on
# Apple's grid: an 824/1024 body with a transparent margin, so widen the viewBox to 512*1024/824.
MAC="$TMP/mac.svg"
sed 's/viewBox="0 0 512 512"/viewBox="-62.136 -62.136 636.272 636.272"/' "$SVG" >"$MAC"
grep -q 'viewBox="-62.136' "$MAC" || { echo "error: expected viewBox=\"0 0 512 512\" in $SVG" >&2; exit 1; }

render() { resvg -w "$2" -h "$2" "$1" "$3" </dev/null; }

render "$SVG" 1024 "$DIR/$SLUG-1024.png"
# Runtime window/Dock icon on macOS (embedded by apps/soundcraft); other platforms use hicolor 256.
render "$MAC" 512 "$DIR/$SLUG-macos-512.png"

# Linux hicolor theme.
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$SVG" "$s" "$DIR/hicolor/${s}x${s}/apps/$ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$SVG" "$DIR/hicolor/scalable/apps/$ID.svg"

# Windows .ico.
ICO_PNGS=()
for s in 16 20 24 32 40 48 64 128 256; do
  render "$SVG" "$s" "$TMP/ico-$s.png"
  ICO_PNGS+=("$TMP/ico-$s.png")
done
if [ "${ICO_PACKER:-python}" = xtask ]; then
  (cd "$ROOT" && cargo run -q -p xtask -- ico "$DIR/$SLUG.ico" "${ICO_PNGS[@]}")
else
  python3 "$ROOT/packaging/icons.py" ico "$DIR/$SLUG.ico" "${ICO_PNGS[@]}"
fi

# macOS .iconset (committed, so a Mac can rebuild the .icns with one command) and .icns.
SET="$DIR/$NAME.iconset"
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  render "$MAC" "$s" "$SET/icon_${s}x${s}.png"
  render "$MAC" $((s * 2)) "$SET/icon_${s}x${s}@2x.png"
done
if command -v iconutil >/dev/null; then
  iconutil -c icns -o "$DIR/$NAME.icns" "$SET"
else
  python3 "$ROOT/packaging/icons.py" icns "$DIR/$NAME.icns" "$SET"
  echo "note: iconutil not found (macOS only); \"$NAME.icns\" was packed by packaging/icons.py" >&2
fi
echo "icons written to $DIR"
