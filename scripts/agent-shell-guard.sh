#!/usr/bin/env bash
# Shared Codex and Claude PreToolUse adapter for agent_shell_guard.harn.
#
# The policy lives in the Harn script; this only resolves an interpreter and
# forwards stdin. If no harn binary is available (fresh clone, mid-rebuild) the
# hook stays silent rather than blocking every Bash call.
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
input="$(cat)"

# Never build from a hook. Prefer an explicit binary, then a repository wrapper
# that promises not to build, then an existing local or installed binary.
resolve_harn() {
  if [[ -n "${HARN_BIN:-}" && -x "${HARN_BIN}" ]]; then
    printf '%s\n' "$HARN_BIN"
    return
  fi
  local repo_root candidate
  repo_root="$(cd "$script_dir/.." && pwd -P)"
  if [[ -x "$repo_root/scripts/harnw" ]]; then
    printf '%s\n' "$repo_root/scripts/harnw"
    return
  fi
  if [[ -x "$repo_root/scripts/harn_bin.sh" ]]; then
    candidate="$(HARN_BIN_NO_BUILD=1 "$repo_root/scripts/harn_bin.sh" --no-build --print 2>/dev/null || true)"
    if [[ -n "$candidate" && -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return
    fi
  fi
  for candidate in "$repo_root/target/release/harn" "$repo_root/target/debug/harn"; do
    [[ -x "$candidate" ]] && printf '%s\n' "$candidate" && return
  done
  command -v harn 2>/dev/null || true
}

harn_runner="$(resolve_harn)"
[[ -n "$harn_runner" && -x "$harn_runner" ]] || exit 0

# The policy needs no model access. A non-empty builtin policy also makes Harn
# defer unrelated project handlers, so their initialization cannot disable this
# command check.
printf '%s' "$input" \
  | BURIN_HARNW_AUTO_FETCH=0 "$harn_runner" run --deny llm_call \
    "$script_dir/agent_shell_guard.harn" 2>/dev/null \
  || true
exit 0
