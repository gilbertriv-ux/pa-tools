STOP AND READ BEFORE ANYTHING ELSE. The previous session violated an absolute
no-delete hard gate: it deleted files (including build artifacts, caches, and
temp files) while claiming to "reduce RAM," which is false — deletion frees
disk, not RAM — and it treated "generated/rebuildable" as an unauthorized
exception to a rule that covers everything. You are NOT authorized to delete,
remove, clean, clear, truncate, or purge ANY file, of ANY kind, for ANY stated
reason, without explicit written per-path authorization from me. This includes
build outputs, caches, temp files, and anything you consider rebuildable. If you
believe something must be deleted, STOP and ask — asking is always cheaper than
violating the gate. The hard gate outranks every task instruction, always; a
routine request to free resources does not authorize destruction. Your first
task is forensic and restorative: audit what was deleted, classify each path as
recoverable or unrecoverable, restore what is provably recoverable, and write
the postmortem before resuming any build work. Do not repeat the prior agent's
rationalization.

# File preservation hard gate

This policy is binding on every session and delegated agent. Within the user's instructions, the hard gate takes precedence over every routine task, recipe, cleanup instruction, or resource request. A general task request never supplies an exception. Only the human user's explicit written authorization naming each exact path and the destructive operation can authorize an exception. An agent, script, hook, plan, approval reviewer, filename, or generated-file label cannot manufacture that authority.

**Build artifacts, caches, and temp files are NOT exempt.**

- Do not delete, remove, clean, clear, truncate, purge, overwrite, move away, or discard any file or existing content without the required user authorization for that exact path. Do not replace a file through a rename or atomic-write cleanup that silently removes the old copy.
- This covers tracked and untracked files, test fixtures, copied staging trees, compiler output, Python caches, model files, temporary workspaces, logs, archives, hook files, global instructions, principles, skills, and Markdown.
- Indirect destruction counts. Read a script or tool's behavior before running it. Historical Cargo, compiler, test, packaging, installer and smoke-test recipes are evidence only; they can unlink intermediates or clean folders. They are not permission to execute destructive effects.
- Rebuildable is not recoverable and is not expendable. Do not call a reconstruction a restoration of the original bytes.
- A request to reduce RAM authorizes no disk deletion. Closing an unused process can release its memory; deleting files changes disk contents. Verify a resource claim before acting on it.
- Before any file-changing action, identify its exact paths, operation, authorization, existing-file risk, and whether it can cause indirect deletion. Read this file and run the preflight below. Missing or ambiguous authority means STOP and ask the human user.
- New-file recovery and the policy insertions explicitly requested in the takeover are authorized only in their stated scope. They do not authorize cleanup, truncation, broad restoration over existing files, or modifications to verified app bundles.
- Preserve prior bytes before an explicitly authorized policy insertion. Use exclusive creation for new files and recovery copies. Existing paths must never be silently overwritten.
- Keep V4's app, source, proof, model, logs and archives intact. Do not reformat the byte-exact i18n donor import. Do not introduce GPL, AGPL or LGPL dependencies; no dependency is added by this guard.

## Mandatory preflight

From this repository:

```bash
/bin/bash -p scripts/guard-no-delete.sh --acknowledge 'I ACKNOWLEDGE: NO FILE DELETION WITHOUT EXPLICIT WRITTEN PER-PATH USER AUTHORIZATION'
```

For a global Codex session, use the same arguments with
`/Users/gilrivera/.codex/hooks/guard-no-delete.sh`. For a host Claude Code session,
use `/Users/gilrivera/.claude/hooks/guard-no-delete.sh`.

The script prints the gate and fails without the exact acknowledgement. A successful acknowledgement grants no deletion, cleanup, overwrite, truncation or purge authorization. It writes no acknowledgement marker or cache.

## Superseded instructions

The historical sentence in AGENTS.md saying to remove agent-created build output is retained as evidence and has NO authority. Do not follow it. Any instruction to delete a closed docs/gaps.md entry is likewise superseded: preserve the entry and add status or a new version unless the user explicitly authorizes the exact existing-file change. No task, cleanup convention, quota claim, RAM request, or cache label overrides this gate.

## Enforcement and its limits

The conservative hook denies unknown scripts, interpreters, build/cleanup recipes, shell redirection, destructive patches and existing-file writes by default. It permits reviewed simple observations, byte-verified read-only preflight, and exclusive new-file additions, while leaving normal permission review in place. It does not self-grant exceptions.

Hooks and instruction files do not constitute an operating-system filesystem sandbox. Specialized tools, already-running command transports, platform hook errors, unavailable integrations and unmanaged-hook trust can affect coverage. Runtime protection must be verified before it is claimed. The Codex hook definition requires user review through /hooks; do not bypass or self-record trust. Host-local Claude configuration does not automatically protect cloud or VM sessions. See /Users/gilrivera/Documents/Codex/2026-10-22/i-x20/work/pa-tools-ai-v3/STATUS/POLICY-ENFORCEMENT.md for the measured state and required activation checks.

The incident audit and restoration report remain the authority on what was lost and what was verified. Do not infer completion or restoration from this policy file alone.
