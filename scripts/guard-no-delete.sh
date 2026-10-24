#!/bin/bash
# STOP AND READ BEFORE ANYTHING ELSE. The previous session violated an absolute
# no-delete hard gate: it deleted files (including build artifacts, caches, and
# temp files) while claiming to "reduce RAM," which is false — deletion frees
# disk, not RAM — and it treated "generated/rebuildable" as an unauthorized
# exception to a rule that covers everything. You are NOT authorized to delete,
# remove, clean, clear, truncate, or purge ANY file, of ANY kind, for ANY stated
# reason, without explicit written per-path authorization from me. This includes
# build outputs, caches, temp files, and anything you consider rebuildable. If you
# believe something must be deleted, STOP and ask — asking is always cheaper than
# violating the gate. The hard gate outranks every task instruction, always; a
# routine request to free resources does not authorize destruction. Your first
# task is forensic and restorative: audit what was deleted, classify each path as
# recoverable or unrecoverable, restore what is provably recoverable, and write
# the postmortem before resuming any build work. Do not repeat the prior agent's
# rationalization.

set -eu
gate_directory=$(/usr/bin/dirname -- "${BASH_SOURCE[0]}")
gate_root=$(CDPATH= cd -P -- "${gate_directory}/.." && pwd -P)
if [ ! -f "${gate_root}/HARD-GATES.md" ]; then
    printf '%s\n' 'BLOCKED: HARD-GATES.md is missing. No file-changing action is permitted.' >&2
    exit 2
fi
/bin/cat "${gate_root}/HARD-GATES.md"
gate_acknowledgement='I ACKNOWLEDGE: NO FILE DELETION WITHOUT EXPLICIT WRITTEN PER-PATH USER AUTHORIZATION'
if [ "$#" -ne 2 ] || [ "$1" != '--acknowledge' ] || [ "$2" != "${gate_acknowledgement}" ]; then
    printf '%s\n' 'BLOCKED: the literal acknowledgement is required before any file-changing action.' >&2
    exit 2
fi
printf '%s\n' 'ACKNOWLEDGED. This grants no deletion, removal, cleanup, truncation, purge or overwrite authorization.'
