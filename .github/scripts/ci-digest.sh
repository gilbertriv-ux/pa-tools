#!/usr/bin/env bash
# Turn the captured log of a failed CI step into one error annotation.
#
# Why: the full job logs are served from a separate blob host, which some sandboxes and proxies
# cannot reach. Annotations travel with the check run itself, so a failure stays readable from
#   GET /repos/<owner>/<repo>/check-runs/<id>/annotations
# and from the run summary page. It also puts the cause on the PR without opening the log.
#
# usage: ci-digest.sh <log-file> [title]      (title: no commas or colons)
set -u

log="${1:?usage: ci-digest.sh <log-file> [title]}"
title="${2:-step failed}"

if [ ! -s "$log" ]; then
  echo "::error title=${title}::no log was captured"
  exit 0
fi

# Strip colour codes and carriage returns, cap the line width. (printf, not a \x1b escape: the
# BSD sed on macOS does not know \x1b.)
esc=$(printf '\033')
clean=$(tr -d '\r' <"$log" | sed -E "s/${esc}\\[[0-9;]*[A-Za-z]//g" | cut -c1-200)

digest=$(
  {
    echo "[compiler errors]"
    printf '%s\n' "$clean" | grep -E -A6 -m4 '^error' | head -n 36
    echo "[failed tests]"
    printf '%s\n' "$clean" | grep -E '^test .* FAILED$|^failures:$|^    [A-Za-z0-9_:]+$|^test result: FAILED' | head -n 40
    echo "[panics]"
    printf '%s\n' "$clean" | grep -E -A2 -m8 'panicked at' | head -n 30
    echo "[last lines]"
    printf '%s\n' "$clean" | tail -n 30
  } || true
)

# Workflow-command escaping: % first, then line breaks.
message=$(printf '%s\n' "$digest" | awk '{ gsub(/%/, "%25"); gsub(/\r/, "%0D"); printf "%s%%0A", $0 }')
echo "::error title=${title}::${message}"
