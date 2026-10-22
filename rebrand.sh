#!/usr/bin/env bash
# Re-apply the Pa Tools brand to the packaging, docs and CI files after merging upstream
# (SoundCraft). Idempotent: on an already rebranded tree it changes nothing.
#
#   ./rebrand.sh            rewrite files in place, then print what still needs a human
#   ./rebrand.sh --check    change nothing; list the files that would change (exit 1 if any)
#   ./rebrand.sh --remove-brand   also `git rm` upstream's brand marks if a merge brought them back
#
# What it rewrites (see UPSTREAM.md for the merge routine):
#   - packaging/**, .github/**, docs/*.md (not the generated docs/parity-checklist.md), ROADMAP.md,
#     sonar-project.properties: upstream names and ids -> the Pa Tools brand below
#   - the generated brand blocks (">>> brand ... <<< brand") in packaging/env.sh, packaging/icons.sh
#     and packaging/windows/package.ps1
#   - the first `name:` of every workflow gets the "Pa Tools" prefix
# What it never touches: Rust, Cargo files, README.md, AGENTS.md, CLAUDE.md, ATTRIBUTION.md, NOTICE,
# UPSTREAM.md, the licence files, contributors/. Those are edited by hand (Rust strings go through
# crates/brand). Lines that credit upstream are left alone: any line containing "upstream-ok",
# "Based on SoundCraft", "inherited from SoundCraft", "upstream SoundCraft" or "by the ArtCraft team".
#
# Needs bash, perl and git. Works with GNU and BSD userlands (no sed -i).
set -euo pipefail

# ---- the brand: edit here, then run ./rebrand.sh ------------------------------------------------
APP_NAME="Pa Tools"
APP_ID="com.imagescouture.patools"
BIN="pa-tools"
CLI="pa-tools-cli"
VENDOR="GR8 ID YUZ.AI"
HOMEPAGE="https://gr8idyuz.ai"                  # PLACEHOLDER until Gil confirms the real site
# ---- derived or secondary -----------------------------------------------------------------------
SLUG="patools"                                  # artifact, icon and web-zip prefix
PROGID="PaTools"                                # Windows ProgId prefix, macOS plist key prefix
MIME="application/x-patools-session"
REPO="gilbertriv-ux/pa-tools"                   # PLACEHOLDER GitHub repository (owner/name)
# -------------------------------------------------------------------------------------------------

MODE=apply
REMOVE_BRAND=0
for arg in "$@"; do
  case "$arg" in
    --check) MODE=check ;;
    --remove-brand) REMOVE_BRAND=1 ;;
    -h | --help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
CHANGED=()

export RB_APP_NAME="$APP_NAME" RB_APP_ID="$APP_ID" RB_BIN="$BIN" RB_CLI="$CLI" RB_VENDOR="$VENDOR" \
  RB_HOMEPAGE="$HOMEPAGE" RB_SLUG="$SLUG" RB_PROGID="$PROGID" RB_MIME="$MIME" \
  RB_REPO="$REPO" RB_REPO_OWNER="${REPO%%/*}" RB_REPO_NAME="${REPO#*/}"

# The files in scope: tracked or new, text only.
in_scope() {
  case "$1" in
    docs/parity-checklist.md | packaging/macos/dmg/background.svg) return 1 ;;
    packaging/* | .github/* | ROADMAP.md | sonar-project.properties) ;;
    docs/*.md) ;;
    *) return 1 ;;
  esac
  [ -f "$1" ] && grep -Iq . "$1"
}

# Rewrite $1 through the perl program $2 (reads stdin, writes stdout); record it if it changed.
apply() {
  local file="$1" prog="$2" out="$TMP/out"
  perl -e "$prog" <"$file" >"$out"
  if ! cmp -s "$file" "$out"; then
    CHANGED+=("$file")
    if [ "$MODE" = apply ]; then cat "$out" >"$file"; fi
  fi
}

# ---- 1. text substitutions ----------------------------------------------------------------------
# Order matters: specific forms first, the plain display name last. Crate and package names
# (soundcraft-engine, soundcraft_mix, -p soundcraft, apps/soundcraft-cli) are never touched.
TEXT_RULES='
use strict; use warnings;
my %v = map { $_ => $ENV{"RB_$_"} } qw(APP_NAME APP_ID BIN CLI VENDOR HOMEPAGE SLUG PROGID MIME REPO REPO_OWNER REPO_NAME);
my $credit = qr/Based on SoundCraft|inherited from SoundCraft|upstream SoundCraft|by the ArtCraft team/;
my $named = 0;
my $is_workflow = ($ENV{RB_FILE} =~ m{^\.github/workflows/});
my $is_sonar = ($ENV{RB_FILE} eq "sonar-project.properties");
while (my $l = <STDIN>) {
  if ($l !~ /upstream-ok/) {
    # publisher, homepage and repository
    $l =~ s{https://getartcraft\.com(?:/apps(?:/soundcraft)?)?/?(?=[\s"<)`]|$)}{$v{HOMEPAGE}}g;
    $l =~ s{https://discord\.gg/artcraft}{$v{HOMEPAGE}}g;
    $l =~ s{Learning Machines LLC}{$v{VENDOR}}g;
    $l =~ s{github\.com/storytold/soundcraft/(issues|releases|blob|tree|actions)}{github.com/$v{REPO}/$1}g;
    $l =~ s{raw\.githubusercontent\.com/storytold/soundcraft/}{raw.githubusercontent.com/$v{REPO}/}g;
    $l =~ s{(GITHUB_REPOSITORY:-)storytold/soundcraft}{$1$v{REPO}}g;
    $l =~ s{https://github\.com/storytold/soundcraft(?=[\s"<)`]|$)}{https://github.com/$v{REPO}}g;
    $l =~ s{gh-releases-zsync\|storytold\|soundcraft\|}{gh-releases-zsync|$v{REPO_OWNER}|$v{REPO_NAME}|}g;
    if ($is_sonar) {
      $l =~ s{^(sonar\.projectKey=).*}{$1$v{REPO_OWNER}_$v{REPO_NAME}};
      $l =~ s{^(sonar\.organization=).*}{$1$v{REPO_OWNER}};
    }
    if ($is_workflow) {
      $l =~ s{^(\s*app-name: )soundcraft\b}{$1$v{SLUG}};
      if (!$named && $l =~ /^name: (?!Pa Tools)/) { $l =~ s{^name: }{name: $v{APP_NAME} }; $named = 1; }
    }
    # ids, types, protocols
    $l =~ s{ai\.storyteller\.soundcraft}{$v{APP_ID}}g;
    $l =~ s{application/x-soundcraft-session}{$v{MIME}}g;
    $l =~ s{soundcraft://}{$v{SLUG}://}g;
    if ($l !~ $credit) {
      $l =~ s{SoundCraft\.(Session|Audio|Midi)\b}{$v{PROGID}.$1}g;
      $l =~ s{SoundCraft(Version|BuildCommit|Data)\b}{$v{PROGID}$1}g;
    }
    # binaries and artifact names (crate names and paths such as apps/soundcraft-cli are skipped)
    $l =~ s{(?<!apps/)(?<!-p )(?<![\w.-])soundcraft-cli(?![\w/])(?! \()}{$v{CLI}}g;
    $l =~ s{(?<![\w.-])soundcraft-web-(?=<v(?:er(?:sion)?)?>|\$\{?VERSION)}{$v{SLUG}-web-}g;
    $l =~ s{(?<![\w.-])soundcraft-(?=<v(?:er(?:sion)?)?>|\$\{?VERSION|\*-)}{$v{SLUG}-}g;
    $l =~ s{(?<![\w.-])soundcraft-(1024|macos-512)\.png}{$v{SLUG}-$1.png}g;
    $l =~ s{(?<![\w.-])soundcraft\.(wxs|ico)\b}{$v{SLUG}.$1}g;
    $l =~ s{(?<![\w.-])soundcraft\.icns\b}{$v{APP_NAME}.icns}g;
    $l =~ s{(?<![\w.-])soundcraft\.exe\b}{$v{BIN}.exe}g;
    $l =~ s{(?<!-p )(?<![\w.-])soundcraft(?= --)}{$v{BIN}}g;
    $l =~ s{mcp add soundcraft\b}{mcp add $v{BIN}}g;
    $l =~ s{"soundcraft"}{"$v{BIN}"}g;
    # display name
    if ($l !~ $credit) {
      $l =~ s{SoundCraft\x27s\b}{$v{APP_NAME}\x27}g;
      $l =~ s{\bSoundCraft\b}{$v{APP_NAME}}g;
    }
  }
  print $l;
}
'

# ---- 2. generated brand blocks -------------------------------------------------------------------
BLOCK_RULE='
use strict; use warnings;
local $/; my $s = <STDIN>; my $b = $ENV{RB_BLOCK};
$s =~ s{(^[^\n]*>>> brand[^\n]*\n).*?(^[^\n]*<<< brand[^\n]*\n)}{$1$b$2}ms;
print $s;
'
sync_block() {
  local file="$1" block="$2" out="$TMP/block.out"
  [ -f "$file" ] || return 0
  RB_BLOCK="$block" perl -e "$BLOCK_RULE" <"$file" >"$out"
  if ! cmp -s "$file" "$out"; then
    CHANGED+=("$file")
    if [ "$MODE" = apply ]; then cat "$out" >"$file"; fi
  fi
}

while IFS= read -r f; do
  in_scope "$f" || continue
  RB_FILE="$f" apply "$f" "$TEXT_RULES"
done < <(git ls-files --cached --others --exclude-standard | sort -u)

sync_block packaging/env.sh "PT_NAME=\"$APP_NAME\"
PT_ID=\"$APP_ID\"
PT_BIN=\"$BIN\"
PT_CLI=\"$CLI\"
PT_SLUG=\"$SLUG\"
PT_VENDOR=\"$VENDOR\"
PT_HOMEPAGE=\"$HOMEPAGE\"
PT_REPO=\"$REPO\"
"
sync_block packaging/icons.sh "ID=\"$APP_ID\"
NAME=\"$APP_NAME\"
SLUG=\"$SLUG\"
"
sync_block packaging/windows/package.ps1 "\$PtName = '$APP_NAME'
\$PtBin = '$BIN'
\$PtCli = '$CLI'
\$PtSlug = '$SLUG'
"

if [ "$MODE" = check ]; then
  if [ "${#CHANGED[@]}" -gt 0 ]; then
    echo "rebrand.sh --check: ${#CHANGED[@]} file(s) would change:"
    printf '  %s\n' "${CHANGED[@]}"
    exit 1
  fi
  echo "rebrand.sh --check: nothing to change"
else
  echo "rebrand.sh: ${#CHANGED[@]} file(s) changed"
  [ "${#CHANGED[@]}" -eq 0 ] || printf '  %s\n' "${CHANGED[@]}"
fi

# ---- 3. what a human still has to look at ------------------------------------------------------
REVIEW=0
note() { echo "review: $*"; REVIEW=1; }

# Upstream brand files that must not come back (the brand licence: a fork removes the marks).
BRAND_FILES=()
while IFS= read -r f; do BRAND_FILES+=("$f"); done < <(git ls-files --cached --others --exclude-standard -- 'docs/brand/*' 2>/dev/null | sort -u)
if [ "${#BRAND_FILES[@]}" -gt 0 ]; then
  if [ "$REMOVE_BRAND" = 1 ] && [ "$MODE" = apply ]; then
    git rm -q -f -- "${BRAND_FILES[@]}" 2>/dev/null || rm -f -- "${BRAND_FILES[@]}"
    echo "removed upstream brand files: ${BRAND_FILES[*]}"
  else
    note "upstream brand files are present (must be removed; rerun with --remove-brand): ${BRAND_FILES[*]}"
  fi
fi

# Files that upstream still names with the old brand: a merge brought them back.
for old in packaging/linux/ai.storyteller.soundcraft.* packaging/linux/flatpak/ai.storyteller.soundcraft*.yml \
  packaging/windows/soundcraft.wxs assets/app-icon/soundcraft* assets/app-icon/hicolor/*/apps/ai.storyteller.soundcraft.*; do
  [ -e "$old" ] && note "old-named file came back from upstream, merge it into the Pa Tools file and delete it: $old"
done

# Literals that are not generated: they must agree with the table at the top.
expect() { # file, fixed string, what
  [ -f "$1" ] || { note "$1 is missing"; return; }
  grep -qF -- "$2" "$1" || note "$1 does not contain $3 ($2): update it by hand"
}
expect packaging/macos/Info.plist.in "<string>$APP_NAME</string>" "the app name"
expect apps/soundcraft-web/index.html "Loading $APP_NAME&hellip;" "the web loading text"
expect packaging/macos/Info.plist.in "<string>$APP_ID</string>" "the bundle id"
expect packaging/macos/Info.plist.in "<string>$APP_ID.session</string>" "the session UTI"
expect packaging/macos/Info.plist.in "<string>$MIME</string>" "the session MIME type"
expect packaging/macos/entitlements.plist "com.apple.security.cs.disable-library-validation" "the library-validation entitlement (G-05)"
expect packaging/windows/patools.wxs "Name=\"$APP_NAME\"" "the product name"
expect packaging/windows/patools.wxs "Manufacturer=\"$VENDOR\"" "the manufacturer"
expect packaging/windows/patools.wxs "\\$BIN.exe" "the app exe name"
expect packaging/windows/patools.wxs "\\$CLI.exe" "the CLI exe name"
expect packaging/windows/patools.wxs "Id=\"$PROGID.Session\"" "the session ProgId"
expect "packaging/linux/$APP_ID.desktop" "Name=$APP_NAME" "the app name"
expect "packaging/linux/$APP_ID.desktop" "Exec=$BIN %F" "the executable"
expect "packaging/linux/$APP_ID.desktop" "StartupWMClass=$APP_ID" "the window class"
expect "packaging/linux/$APP_ID.metainfo.xml.in" "<id>$APP_ID</id>" "the AppStream id"
expect packaging/linux/nfpm.yaml "vendor: $VENDOR" "the vendor"
expect packaging/linux/nfpm.yaml "dst: /usr/bin/$BIN" "the executable"
expect packaging/linux/nfpm.yaml "dst: /usr/bin/$CLI" "the CLI"
expect "packaging/linux/flatpak/$APP_ID.yml" "id: $APP_ID" "the Flatpak id"
expect "packaging/linux/flatpak/$APP_ID.bundle.yml" "command: $BIN" "the Flatpak command"
expect packaging/macos/dmg/generate.py "VOLUME = \"$APP_NAME\"" "the DMG volume name"
expect packaging/macos/dmg/generate.py "\"$APP_NAME.app\"" "the DMG layout app name"
expect "assets/app-icon/$APP_NAME.icns" "icns" "the macOS icon"

# Remaining upstream names in the files above, minus crate names, env vars and credit lines.
LEFT="$TMP/left"
while IFS= read -r f; do
  in_scope "$f" || continue
  grep -n -i -E 'soundcraft|storyteller|artcraft|getartcraft|learning machines|discord\.gg' "$f" 2>/dev/null |
    grep -v -E 'upstream-ok|Based on SoundCraft|inherited from SoundCraft|upstream SoundCraft|by the ArtCraft team' |
    perl -ne 's{apps/soundcraft(?:-cli|-web)?}{}g; s{-p soundcraft(?:-cli|-web)?}{}g; s{SOUNDCRAFT_[\w*]*}{}g; s{soundcraft(?= \()}{}g; s{soundcraft[-_](?:time|audio[-_]io|midi|video|dsp|clap[-_]host|vst3[-_]host|au[-_]host|model|mix|engine|playback|automation|ui[-_]egui|brand|web|cli)\b}{}g; s{storytold/soundcraft}{}g; print if /soundcraft|storyteller|artcraft|getartcraft|learning machines|discord\.gg/i' |
    sed "s|^|$f:|" >>"$LEFT" || true
done < <(git ls-files --cached --others --exclude-standard | sort -u)
if [ -s "$LEFT" ]; then
  note "upstream names left in rebranded files (crate names and credits are filtered out):"
  sed 's/^/  /' "$LEFT"
fi

if [ "$REVIEW" = 0 ]; then echo "rebrand.sh: nothing left to review"; fi
