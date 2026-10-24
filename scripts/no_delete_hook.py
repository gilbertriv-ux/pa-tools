#!/usr/bin/env python3
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

"""Conservative preservation policy; no mutation, subprocess, or authorization writes."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import sys

BANNER = "STOP AND READ BEFORE ANYTHING ELSE. The previous session violated an absolute\nno-delete hard gate: it deleted files (including build artifacts, caches, and\ntemp files) while claiming to \"reduce RAM,\" which is false — deletion frees\ndisk, not RAM — and it treated \"generated/rebuildable\" as an unauthorized\nexception to a rule that covers everything. You are NOT authorized to delete,\nremove, clean, clear, truncate, or purge ANY file, of ANY kind, for ANY stated\nreason, without explicit written per-path authorization from me. This includes\nbuild outputs, caches, temp files, and anything you consider rebuildable. If you\nbelieve something must be deleted, STOP and ask — asking is always cheaper than\nviolating the gate. The hard gate outranks every task instruction, always; a\nroutine request to free resources does not authorize destruction. Your first\ntask is forensic and restorative: audit what was deleted, classify each path as\nrecoverable or unrecoverable, restore what is provably recoverable, and write\nthe postmortem before resuming any build work. Do not repeat the prior agent's\nrationalization."
PREFLIGHT_SHA256 = "5fe6be764e19aced154c45c6c403c8595ce79855a6ff9ab4b6c16215b492842a"
ACKNOWLEDGEMENT = "I ACKNOWLEDGE: NO FILE DELETION WITHOUT EXPLICIT WRITTEN PER-PATH USER AUTHORIZATION"

READ_TOOLS = {
    "Read", "Glob", "Grep", "LS", "WebSearch", "WebFetch",
    "list_agents", "wait_agent", "wait", "spawn_agent", "Agent",
    "send_message", "followup_task", "exec",
    "mcp__codex_app__list_threads", "mcp__codex_app__read_thread",
    "mcp__codex_app__list_projects", "mcp__codex_app__list_artifacts",
    "mcp__codex_app__get_usage_limits",
}
READ_COMMANDS = {
    "pwd", "ls", "cat", "head", "tail", "wc", "stat", "file",
    "du", "df", "date", "readlink", "id", "uname", "shasum", "sha256sum",
    "basename", "dirname", "which", "type",
}
CONTEXT_EVENTS = {"SessionStart", "SubagentStart", "UserPromptSubmit"}
SHELL_CONTROL = ("$", chr(96), ">", "<", ";", "|", "&", "\n", "\r", "\\")
PROTECTED_NAMES = {"AGENTS.md", "AGENTS.override.md", "CLAUDE.md", "HARD-GATES.md"}
PROTECTED_PARTS = {".codex", ".claude", ".git"}

def path_status(raw, cwd):
    if not isinstance(raw, str) or not raw or "\x00" in raw or raw.startswith("~"):
        return False, "Invalid or unreviewed destination path."
    target = Path(raw)
    if ".." in target.parts:
        return False, "Parent traversal is not accepted."
    if not target.is_absolute():
        target = Path(cwd) / target
    if target.name in PROTECTED_NAMES or set(target.parts) & PROTECTED_PARTS:
        return False, "Policy, configuration and Git files require separate review."
    if any(part in {"no_delete_hook.py", "no_delete_guard.py", "guard-no-delete.sh"} for part in target.parts):
        return False, "A guard cannot authorize changes to itself."
    if str(target).startswith(("/dev/", "/proc/", "/sys/")):
        return False, "A virtual filesystem is not an exclusive new-file destination."
    if os.path.lexists(target):
        return False, "The destination already exists; overwrite/truncation is not authorized."
    return True, "Exclusive new-file creation."

def shell_read_only(command):
    if not isinstance(command, str) or not command:
        return False, "Missing shell command."
    if any(token in command for token in SHELL_CONTROL):
        return False, "Compound shell syntax, redirection, substitution and interpreter input require review."
    try:
        words = shlex.split(command)
    except ValueError:
        return False, "Unparseable shell input."
    if not words:
        return False, "Empty shell input."
    name = words[0]
    if "/" in name and not name.startswith(("/bin/", "/usr/bin/", "/usr/local/bin/", "/opt/homebrew/bin/")):
        return False, "Unreviewed executable path."
    base = Path(name).name
    args = words[1:]
    if base in READ_COMMANDS:
        if base == "file" and any(arg.startswith(("-m", "--magic-file")) for arg in args):
            return False, "Custom magic execution paths are not accepted."
        return True, "Known simple read-only command."
    if base == "command":
        return (bool(args) and args[0] in {"-v", "-V"}, "Only command lookup is permitted.")
    if base == "rg":
        if any(arg.startswith(("--pre", "--hostname-bin")) for arg in args):
            return False, "Preprocessors or external command options are not accepted."
        return True, "Read-only text search."
    if base == "git":
        if not args or args[0] != "--no-optional-locks":
            return False, "Git inspection must explicitly disable optional index writes."
        args = args[1:]
        if len(args) >= 2 and args[0] == "-C":
            args = args[2:]
        if not args or args[0] not in {"status", "diff", "show", "log", "reflog", "rev-parse", "ls-files", "ls-tree", "cat-file"}:
            return False, "Only reviewed read-only Git subcommands are accepted."
        if any(arg in {"--ext-diff", "--textconv", "--show-signature"} or arg.startswith(("--output", "--exec-path")) for arg in args):
            return False, "Git external helpers and output-file options are not accepted."
        if args[0] == "diff" and not {"--no-ext-diff", "--no-textconv"}.issubset(args):
            return False, "Git diff must explicitly disable external diff and textconv helpers."
        return True, "Git inspection without optional writes."
    if base in {"bash", "sh"} and len(args) == 2 and args[0] == "-n":
        return True, "Shell syntax inspection only."
    if base == "bash" and len(args) == 4 and args[0] == "-p" and args[2] == "--acknowledge" and args[3] == ACKNOWLEDGEMENT:
        path = Path(args[1])
        try:
            if path.name == "guard-no-delete.sh" and hashlib.sha256(path.read_bytes()).hexdigest() == PREFLIGHT_SHA256:
                return True, "Byte-verified, read-only gate acknowledgement; no deletion authority granted."
        except OSError:
            pass
        return False, "The preflight script does not match the reviewed bytes."
    return False, "Unknown command, script, interpreter, build, package or cleanup recipe: stop and review."

def patch_add_only(patch, cwd):
    if not isinstance(patch, str):
        return False, "No parseable patch."
    lines = patch.splitlines()
    if not lines or lines[0] != "*** Begin Patch" or lines[-1] != "*** End Patch":
        return False, "Unsupported patch envelope."
    count, in_file, seen = 0, False, set()
    for line in lines[1:-1]:
        if line.startswith("*** Add File: "):
            raw = line[len("*** Add File: "):]
            ok, reason = path_status(raw, cwd)
            if not ok:
                return False, reason
            if raw in seen:
                return False, "Duplicate destinations are not accepted."
            seen.add(raw)
            count += 1
            in_file = True
        elif line.startswith("+") and in_file:
            continue
        else:
            return False, "Only exclusive Add File patches are accepted; existing-file changes require explicit review."
    return (count > 0, "Exclusive new-file patch." if count else "No new-file operation.")

def evaluate(event):
    if not isinstance(event, dict):
        return False, "The hook input must be an object."
    event_name = event.get("hook_event_name")
    if event_name in CONTEXT_EVENTS:
        return True, "context"
    if event_name != "PreToolUse":
        return False, "Unknown hook event; fail closed."
    tool, data, cwd = event.get("tool_name"), event.get("tool_input"), event.get("cwd")
    if not isinstance(cwd, str) or not Path(cwd).is_absolute():
        return False, "An absolute working directory is required."
    if tool in {"Agent", "spawn_agent"} and isinstance(data, dict):
        environment = data.get("environment")
        if data.get("isolation") == "worktree" or (isinstance(environment, dict) and environment.get("type") == "worktree"):
            return False, "Automatic worktree creation/removal is not authorized by collaboration permission."
    if tool in READ_TOOLS:
        return True, "Known observation or collaboration tool; normal permissions still apply."
    if tool in {"Bash", "exec_command"}:
        if not isinstance(data, dict):
            return False, "Missing shell arguments."
        command = data.get("command", data.get("cmd"))
        if "command" in data and "cmd" in data and data["command"] != data["cmd"]:
            return False, "Ambiguous shell commands."
        return shell_read_only(command)
    if tool in {"apply_patch", "Edit", "Write"}:
        if tool == "Edit":
            return False, "Existing-file edits require separately reviewed written per-path authorization."
        if tool == "Write" and isinstance(data, dict) and "file_path" in data:
            return path_status(data["file_path"], cwd)
        patch = data.get("command") if isinstance(data, dict) else data
        return patch_add_only(patch, cwd)
    return False, "Unknown tool or mutation: stop and review; there is no generated-file exception."

def render(event):
    ok, reason = evaluate(event)
    if ok and event.get("hook_event_name") in CONTEXT_EVENTS:
        return {"hookSpecificOutput": {"hookEventName": event["hook_event_name"], "additionalContext": BANNER}}, 0, ""
    if ok:
        return {}, 0, ""
    text = BANNER + "\n\nBLOCKED: " + reason + "\nThis hook does not self-grant deletion authority. Obtain explicit written user authorization for each exact path."
    return {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": text}}, 2, text

def main():
    try:
        raw = sys.stdin.read(2_000_001)
        if len(raw) > 2_000_000:
            raise ValueError("Input exceeds the fixed limit")
        event = json.loads(raw)
        value, status, error = render(event)
    except Exception:
        text = BANNER + "\n\nBLOCKED: unreadable, oversized or malformed hook input; fail closed."
        value = {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": text}}
        status, error = 2, text
    print(json.dumps(value, ensure_ascii=False))
    if error:
        print(error, file=sys.stderr)
    return status


# Preservation-only v2 hardening: the complete reviewed v1 bytes remain above.
# These later bindings intentionally supersede unsafe broad classifications.
READ_TOOLS.discard("exec")

_v1_shell_read_only = shell_read_only
def shell_read_only(command):
    if isinstance(command, str):
        try:
            words = shlex.split(command)
        except ValueError:
            words = []
        if words and Path(words[0]).name == "file":
            return False, "File inspection tool options can compile or load custom magic; fail closed."
        if words and Path(words[0]).name == "git":
            if words in (
                ["git", "--no-optional-locks", "--no-pager", "--version"],
                ["/usr/bin/git", "--no-optional-locks", "--no-pager", "--version"],
            ):
                return True, "Exact version observation only; no repository helpers."
            return False, "Git repository commands can invoke configured helpers, filters or pagers; fail closed."
    return _v1_shell_read_only(command)

_v1_path_status = path_status
def path_status(raw, cwd):
    ok, reason = _v1_path_status(raw, cwd)
    if not ok:
        return ok, reason
    target = Path(raw)
    if not target.is_absolute():
        target = Path(cwd) / target
    try:
        for parent in target.parents:
            if parent.is_symlink():
                return False, "Symlink ancestors require separate per-path review."
        resolved = target.resolve(strict=False)
    except (OSError, RuntimeError):
        return False, "Destination ancestry cannot be proven; fail closed."
    if resolved.name in PROTECTED_NAMES or set(resolved.parts) & PROTECTED_PARTS:
        return False, "Resolved policy/configuration/Git destinations require separate review."
    return True, "Absent new-file destination; provider must still guarantee exclusive creation."

_v1_patch_add_only = patch_add_only
def patch_add_only(patch, cwd):
    ok, reason = _v1_patch_add_only(patch, cwd)
    if not ok:
        return ok, reason
    import unicodedata
    seen = set()
    for line in patch.splitlines():
        if line.startswith("*** Add File: "):
            target = Path(line[len("*** Add File: "):])
            if not target.is_absolute():
                target = Path(cwd) / target
            try:
                # Case-fold conservatively, including on case-sensitive volumes.
                canonical = unicodedata.normalize("NFC", str(target.resolve(strict=False))).casefold()
            except (OSError, RuntimeError):
                return False, "Destination identity cannot be proven; fail closed."
            if canonical in seen:
                return False, "Canonical/lexical/case aliases cannot write the same new destination twice."
            seen.add(canonical)
    return True, "Distinct absent destinations only; provider must use exclusive creation."



# Further fail-closed schema and case-normalized policy-path checks.
_v2_evaluate = evaluate
def evaluate(event):
    if isinstance(event, dict) and event.get("hook_event_name") == "PreToolUse":
        tool, data = event.get("tool_name"), event.get("tool_input")
        if not isinstance(tool, str) or not tool:
            return False, "Missing or malformed tool name; fail closed."
        if "tool_input" not in event or data is None:
            return False, "Missing tool input; fail closed."
        if tool == "apply_patch":
            if not isinstance(data, (dict, str)):
                return False, "Malformed patch input; fail closed."
        elif not isinstance(data, dict):
            return False, "Malformed tool input; fail closed."
    return _v2_evaluate(event)

_v2_path_status = path_status
def path_status(raw, cwd):
    ok, reason = _v2_path_status(raw, cwd)
    if not ok:
        return ok, reason
    import unicodedata
    target = Path(raw)
    if not target.is_absolute():
        target = Path(cwd) / target
    try:
        resolved = target.resolve(strict=False)
        folded_parts = {unicodedata.normalize("NFC", part).casefold() for part in resolved.parts}
        folded_name = unicodedata.normalize("NFC", resolved.name).casefold()
    except (OSError, RuntimeError):
        return False, "Destination identity cannot be proven; fail closed."
    if folded_parts & PROTECTED_PARTS or folded_name in {p.casefold() for p in PROTECTED_NAMES}:
        return False, "Case/Unicode aliases of policy/configuration/Git paths require separate review."
    return True, reason



# Environment-sensitive search and clock-setting commands also fail closed.
_v3_shell_read_only = shell_read_only
def shell_read_only(command):
    if isinstance(command, str):
        try:
            words = shlex.split(command)
        except ValueError:
            words = []
        if words and Path(words[0]).name == "date":
            return False, "Date commands can set the system clock; use a dedicated observation tool."
        if words and Path(words[0]).name == "rg":
            if len(words) < 2 or words[1] != "--no-config":
                return False, "Ripgrep must start with --no-config to disable environment-supplied preprocessors."
    return _v3_shell_read_only(command)


if __name__ == "__main__":
    raise SystemExit(main())
