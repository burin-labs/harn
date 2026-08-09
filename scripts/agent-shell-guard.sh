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

# The policy uses core data functions, stdin/stdout, and Harn's deterministic
# command parser. That parser is the only non-core builtin allowed here; the
# model kill switch blocks real provider calls. Project handlers are lazy by
# default, so unrelated handler graphs do not initialize during this check.
run_guard() {
  BURIN_HARNW_AUTO_FETCH=0 HARN_LLM_CALLS_DISABLED=1 "$harn_runner" run \
    --allow=command_risk_scan "$script_dir/agent_shell_guard.harn"
}

if [[ "${AGENT_SHELL_GUARD_DEBUG:-0}" == "1" ]]; then
  printf '%s' "$input" | run_guard || true
else
  printf '%s' "$input" | run_guard 2>/dev/null || true
fi
exit 0
