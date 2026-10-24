#!/usr/bin/env bash
# PreToolUse hook (Bash): refuse delete/move/overwrite commands that touch Gil's computer or drives.
# Cloud-container scratch (/tmp, repo build output) is allowed. Exit 2 = block, message goes to the agent.
cmd="$(cat | python3 -c 'import sys,json
try: print(json.load(sys.stdin).get("tool_input",{}).get("command",""))
except Exception: print("")')"
destructive='(^|[;&|[:space:]])(rm|rmdir|mv|shred|truncate|unlink|trash|dd|rsync[^|;&]*--delete|find[^|;&]*-delete|diskutil[[:space:]]+(erase|partition|apfs[[:space:]]+delete))([[:space:]]|$)'
protected='(/Volumes/|/Users/|~/|\$HOME|/Applications/)'
if printf '%s' "$cmd" | grep -Eq "$destructive" && printf '%s' "$cmd" | grep -Eq "$protected"; then
  echo "BLOCKED by hard gate: never delete, move, overwrite or rename anything on Gil's computer or external drives (/Volumes, /Users, ~, /Applications). Copy to a new path and ask Gil. Cloud /tmp scratch is fine." >&2
  exit 2
fi
exit 0
