#!/usr/bin/env bash
# Shared Codex and Claude PreToolUse adapter for agent_shell_guard.harn.
#
# The policy lives in the Harn script; this only resolves an interpreter and
# forwards stdin. If no harn binary is available (fresh clone, mid-rebuild) the
# hook stays silent rather than blocking every Bash call.
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# The policy reads its payload from a file, not an inherited pipe. The deadline
# below may run it as an asynchronous command, and a non-interactive shell
# reassigns an async command's stdin to /dev/null — which would hand the guard an
# empty payload and silently turn every decision into a pass.
payload_file="$(mktemp "${TMPDIR:-/tmp}/agent-shell-guard.XXXXXX")"
trap 'rm -f "$payload_file"' EXIT
cat >"$payload_file"

# Never build from a hook. Prefer an explicit binary, then a repository wrapper
# that promises not to build, then an existing local or installed binary.
# A cargo `debug` artifact is the wrong interpreter for a hook that runs before
# every shell call. It starts slower than a release build of the same revision,
# and it is the exact file cargo rewrites mid-build, so a resolver that reaches
# for it first makes hook latency track whatever the developer is compiling.
# Nothing requires it: the policy is a `.harn` script read from disk, so policy
# edits take effect under any interpreter, and a developer changing a builtin
# the policy depends on still has the explicit `HARN_BIN` override above.
is_debug_build() {
  case "$1" in
    */debug/harn|*/debug/harn.exe) return 0 ;;
    *) return 1 ;;
  esac
}

resolve_harn() {
  if [[ -n "${HARN_BIN:-}" && -x "${HARN_BIN}" ]]; then
    printf '%s\n' "$HARN_BIN"
    return
  fi
  local repo_root candidate debug_fallback=""
  repo_root="$(cd "$script_dir/.." && pwd -P)"
  if [[ -x "$repo_root/scripts/harnw" ]]; then
    printf '%s\n' "$repo_root/scripts/harnw"
    return
  fi
  if [[ -x "$repo_root/scripts/harn_bin.sh" ]]; then
    candidate="$(HARN_BIN_NO_BUILD=1 "$repo_root/scripts/harn_bin.sh" --no-build --print 2>/dev/null || true)"
    if [[ -n "$candidate" && -x "$candidate" ]]; then
      if is_debug_build "$candidate"; then
        debug_fallback="$candidate"
      else
        printf '%s\n' "$candidate"
        return
      fi
    fi
  fi
  if [[ -x "$repo_root/target/release/harn" ]]; then
    printf '%s\n' "$repo_root/target/release/harn"
    return
  fi
  candidate="$(command -v harn 2>/dev/null || true)"
  if [[ -n "$candidate" ]]; then
    if is_debug_build "$candidate"; then
      [[ -n "$debug_fallback" ]] || debug_fallback="$candidate"
    else
      printf '%s\n' "$candidate"
      return
    fi
  fi
  # No release-grade interpreter anywhere. A debug build still beats no guard.
  [[ -n "$debug_fallback" ]] && printf '%s\n' "$debug_fallback" && return
  [[ -x "$repo_root/target/debug/harn" ]] && printf '%s\n' "$repo_root/target/debug/harn" && return
  return 0
}

harn_runner="$(resolve_harn)"
[[ -n "$harn_runner" && -x "$harn_runner" ]] || exit 0

# The policy uses core data functions, stdin/stdout, and Harn's deterministic
# command parser. That parser is the only non-core builtin allowed here; the
# model kill switch blocks real provider calls. Project handlers are lazy by
# default, so unrelated handler graphs do not initialize during this check.
guard_command=(
  env BURIN_HARNW_AUTO_FETCH=0 HARN_LLM_CALLS_DISABLED=1
  "$harn_runner" run --allow=command_risk_scan
  "$script_dir/agent_shell_guard.harn"
)

# A PreToolUse hook holds the agent's shell call open for as long as it runs, so
# the harness timeout is the wrong backstop: by the time it fires the agent has
# already paid the entire budget AND still has no verdict — maximum latency for
# zero protection. Bound the policy here, well inside the hook budget, and fail
# open the moment it overruns. A guard that cannot answer quickly must get out of
# the way, exactly as a crashed one already does.
deadline_seconds="${AGENT_SHELL_GUARD_DEADLINE_SECONDS:-5}"

run_with_deadline() {
  # `timeout` is coreutils; a stock macOS ships neither it nor `gtimeout`, so
  # fall back to a watchdog needing nothing beyond bash 3.2.
  local timeout_bin
  timeout_bin="$(command -v timeout || command -v gtimeout || true)"
  if [[ -n "$timeout_bin" ]]; then
    "$timeout_bin" "$deadline_seconds" "$@" <"$payload_file"
    return
  fi

  local policy_pid watchdog_pid status=0
  "$@" <"$payload_file" &
  policy_pid=$!
  ( sleep "$deadline_seconds"; kill -TERM "$policy_pid" 2>/dev/null ) &
  watchdog_pid=$!
  wait "$policy_pid" || status=$?
  kill -TERM "$watchdog_pid" 2>/dev/null || true
  wait "$watchdog_pid" 2>/dev/null || true
  return "$status"
}

guard_status=0
if [[ "${AGENT_SHELL_GUARD_DEBUG:-0}" == "1" ]]; then
  run_with_deadline "${guard_command[@]}" || guard_status=$?
else
  run_with_deadline "${guard_command[@]}" 2>/dev/null || guard_status=$?
fi

# 124 is how coreutils `timeout` reports the deadline; 143 is SIGTERM from the
# fallback watchdog. Both mean the same thing: no verdict, so allow the call.
if [[ "$guard_status" == "124" || "$guard_status" == "143" ]] \
  && [[ "${AGENT_SHELL_GUARD_DEBUG:-0}" == "1" ]]; then
  echo "agent-shell-guard: policy exceeded ${deadline_seconds}s; failing open" >&2
fi
exit 0
