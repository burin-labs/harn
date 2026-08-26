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
decision_file="$(mktemp "${TMPDIR:-/tmp}/agent-shell-guard-decision.XXXXXX")"
deadline_marker="${decision_file}.deadline"
trap 'rm -f "$payload_file" "$decision_file" "$deadline_marker"' EXIT
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
    # Explicit overrides may name an older Harn without standalone execution.
    # Keep their project-aware invocation for compatibility.
    printf 'project\t%s\n' "$HARN_BIN"
    return
  fi
  local repo_root candidate debug_fallback="" hook_harn marker capability="" marker_extra="" has_marker_extra=0 capability_terminated=0
  repo_root="$(cd "$script_dir/.." && pwd -P)"
  hook_harn="${AGENT_SHELL_GUARD_HARN_BIN:-${XDG_CACHE_HOME:-${HOME:-}/.cache}/harn/hook-bin/harn}"
  marker="${hook_harn}.standalone-v1"
  if [[ -x "$hook_harn" && -f "$marker" ]]; then
    {
      if IFS= read -r capability; then
        capability_terminated=1
      fi
      if IFS= read -r marker_extra || [[ -n "$marker_extra" ]]; then
        has_marker_extra=1
      fi
    } <"$marker"
    if [[ ( "$capability" == "harn-run-standalone-v1" \
          || ( "$capability" == $'harn-run-standalone-v1\r' \
            && "$capability_terminated" -eq 1 ) ) \
        && "$has_marker_extra" -eq 0 ]]; then
      printf 'standalone\t%s\n' "$hook_harn"
      return
    fi
  fi
  if [[ -x "$repo_root/scripts/harnw" ]]; then
    printf 'project\t%s\n' "$repo_root/scripts/harnw"
    return
  fi
  if [[ -x "$repo_root/scripts/harn_bin.sh" ]]; then
    candidate="$(HARN_BIN_NO_BUILD=1 "$repo_root/scripts/harn_bin.sh" --no-build --print 2>/dev/null || true)"
    if [[ -n "$candidate" && -x "$candidate" ]]; then
      if is_debug_build "$candidate"; then
        debug_fallback="$candidate"
      else
        printf 'project\t%s\n' "$candidate"
        return
      fi
    fi
  fi
  if [[ -x "$repo_root/target/release/harn" ]]; then
    printf 'project\t%s\n' "$repo_root/target/release/harn"
    return
  fi
  candidate="$(command -v harn 2>/dev/null || true)"
  if [[ -n "$candidate" ]]; then
    if is_debug_build "$candidate"; then
      [[ -n "$debug_fallback" ]] || debug_fallback="$candidate"
    else
      printf 'project\t%s\n' "$candidate"
      return
    fi
  fi
  # No release-grade interpreter anywhere. A debug build still beats no guard.
  [[ -n "$debug_fallback" ]] && printf 'project\t%s\n' "$debug_fallback" && return
  [[ -x "$repo_root/target/debug/harn" ]] \
    && printf 'project\t%s\n' "$repo_root/target/debug/harn" \
    && return
  return 0
}

resolved_harn="$(resolve_harn)"
IFS=$'\t' read -r harn_mode harn_runner <<<"$resolved_harn"
[[ -n "$harn_runner" && -x "$harn_runner" ]] || exit 0

# The policy uses core data functions, stdin/stdout, and Harn's deterministic
# command parser. That parser is the only non-core builtin allowed here; the
# model kill switch blocks real provider calls. Standalone mode makes the
# policy independent of ambient project handlers, packages, skills, and cache
# authority; the parser remains an explicit host grant.
guard_command=(
  env HARN_LLM_CALLS_DISABLED=1
  "$harn_runner" run
)
[[ "$harn_mode" == "standalone" ]] && guard_command+=(--standalone)
guard_command+=(--allow=command_risk_scan "$script_dir/agent_shell_guard.harn")

# A PreToolUse hook holds the agent's shell call open for as long as it runs, so
# the harness timeout is the wrong backstop: by the time it fires the agent has
# already paid the entire budget and still has no verdict. Bound the policy
# here, well inside the hook budget. A timeout is an unknown policy decision,
# so deny the command; a crashed or unavailable interpreter remains fail-open
# to keep a broken local installation recoverable.
deadline_seconds="${AGENT_SHELL_GUARD_DEADLINE_SECONDS:-5}"
kill_grace_seconds="${AGENT_SHELL_GUARD_KILL_GRACE_SECONDS:-1}"

run_with_deadline() {
  # Use one bash 3.2-compatible watchdog on every platform. GNU `timeout`
  # stops monitoring after the direct child accepts TERM, so a descendant that
  # ignores TERM can retain the hook pipes and escape `--kill-after`.
  local policy_pid watchdog_pid status=0
  # Job control gives the policy its own process group even on stock macOS,
  # where `setsid` is unavailable. The watchdog can then reach descendants
  # after the interpreter exits or reparents them.
  set -m
  "$@" <"$payload_file" &
  policy_pid=$!
  (
    sleep "$deadline_seconds"
    : >"$deadline_marker"
    kill -TERM -- "-$policy_pid" 2>/dev/null || true
    sleep "$kill_grace_seconds"
    kill -KILL -- "-$policy_pid" 2>/dev/null || true
  ) &
  watchdog_pid=$!
  set +m
  wait "$policy_pid" || status=$?
  if [[ -f "$deadline_marker" ]]; then
    # TERM can end the interpreter while a TERM-ignoring descendant still owns
    # the hook pipes. Let the watchdog finish its KILL phase before returning.
    wait "$watchdog_pid" 2>/dev/null || true
    status=124
  else
    kill -TERM -- "-$watchdog_pid" 2>/dev/null || true
    wait "$watchdog_pid" 2>/dev/null || true
  fi
  rm -f "$deadline_marker"
  return "$status"
}

guard_status=0
if [[ "${AGENT_SHELL_GUARD_DEBUG:-0}" == "1" ]]; then
  run_with_deadline "${guard_command[@]}" >"$decision_file" || guard_status=$?
else
  run_with_deadline "${guard_command[@]}" >"$decision_file" 2>/dev/null \
    || guard_status=$?
fi

# 124 is the conventional deadline status; 137 and 143 are SIGKILL and SIGTERM.
# All mean the same thing here: the deny-class policy did not produce a
# trustworthy verdict, so the adapter fails closed.
if [[ "$guard_status" == "0" ]]; then
  cat "$decision_file"
elif [[ "$guard_status" == "124" || "$guard_status" == "137" || "$guard_status" == "143" ]]; then
  printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Repository command policy timed out before producing a verdict; retry when host load subsides."}}'
  if [[ "${AGENT_SHELL_GUARD_DEBUG:-0}" == "1" ]]; then
    echo "agent-shell-guard: policy exceeded ${deadline_seconds}s; failing closed" >&2
  fi
fi
exit 0
