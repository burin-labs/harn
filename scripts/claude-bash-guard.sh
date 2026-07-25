#!/usr/bin/env bash
# PreToolUse shim for scripts/claude_bash_guard.harn.
#
# The policy lives in the Harn script; this only resolves an interpreter and
# forwards stdin. If no harn binary is available (fresh clone, mid-rebuild) the
# hook stays silent rather than blocking every Bash call.
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
input="$(cat)"

# Never build from a hook: a PreToolUse handler runs before every Bash call, so
# it must resolve in milliseconds or not at all. Take the first binary that
# already exists, newest build profile first.
resolve_harn() {
  if [[ -n "${HARN_BIN:-}" && -x "${HARN_BIN}" ]]; then
    printf '%s\n' "$HARN_BIN"
    return
  fi
  local repo_root candidate
  repo_root="$(cd "$script_dir/.." && pwd -P)"
  for candidate in "$repo_root/target/release/harn" "$repo_root/target/debug/harn"; do
    [[ -x "$candidate" ]] && printf '%s\n' "$candidate" && return
  done
  command -v harn 2>/dev/null || true
}

harn_bin="$(resolve_harn)"
[[ -n "$harn_bin" && -x "$harn_bin" ]] || exit 0

printf '%s' "$input" | "$harn_bin" run "$script_dir/claude_bash_guard.harn" 2>/dev/null || true
exit 0
