#!/usr/bin/env bash
set -euo pipefail

# Read one head commit's CI check state as a typed census.
#
# The Harn entry point runs inside the default worktree sandbox. Grant gh its
# token, temporary config, exact executable files, and any wrapper-owned budget
# state. The wrapper describes its own paths; the launcher keeps the sandbox.
#
# usage: scripts/gh_check_state.sh --repo OWNER/NAME --sha <40-hex> [--base REF]
#                                  [--workflow PATH] [--expect NAME ...] [--json]
#
# Automatic routing expectations apply only to Harn and a workflow owned by
# its repository. Other repositories must supply required checks with --expect.
# Explicit requirements replace routing inference and must finish successfully;
# a skipped required check fails, while unrelated routed skips remain visible.
#
# Exit codes: 0 green, 1 failing, 2 pending, 3 settled with an expected check
# missing or expectations unobservable, 64 usage error.

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# A status read must never acquire compiler capacity. Keep binary selection
# with the shared resolver, but classify unavailable execution as unobservable.
if ! harn_bin="$("$script_dir/harn_bin.sh" --no-build --print)"; then
  echo "gh_check_state: Harn unavailable; no check census was measured" >&2
  exit 3
fi
export HARN_BIN="$harn_bin"

if ! gh_program="$(command -v gh)" || [[ ! -f "$gh_program" ]] \
  || ! gh_program="$(realpath "$gh_program")"; then
  echo "gh_check_state: GitHub CLI unavailable; no check census was measured" >&2
  exit 3
fi
gh_policy_args=(--sandbox-read-root "$gh_program")
# Native gh needs only its executable. Script wrappers own additional paths;
# require their typed contract rather than guessing installation or state roots.
if [[ "$(head -c 2 "$gh_program")" == '#!' ]]; then
  if ! gh_policy="$(GH_BUDGET_PRINT_REAL=1 "$gh_program" --budget-subprocess-policy)" \
    || ! jq -e --arg executable "$gh_program" '
      type == "object" and
      (keys == ["executable", "implementation", "real_executable", "schema", "state_directory"]) and
      .schema == "gh-budget.subprocess-policy.v1" and .executable == $executable and
      ([.executable, .implementation, .real_executable, .state_directory] |
        all(type == "string" and startswith("/") and (explode | all(. >= 32))))
    ' <<< "$gh_policy" >/dev/null; then
    echo "gh_check_state: wrapper subprocess policy unavailable or invalid; no check census was measured" >&2
    exit 3
  fi
  gh_implementation="$(jq -r '.implementation' <<< "$gh_policy")"
  export GH_BUDGET_REAL_GH="$(jq -r '.real_executable' <<< "$gh_policy")"
  export GH_BUDGET_STATE_DIR="$(jq -r '.state_directory' <<< "$gh_policy")"
  if [[ ! -f "$gh_implementation" || ! -f "$GH_BUDGET_REAL_GH" \
    || ! -x "$GH_BUDGET_REAL_GH" ]]; then
    echo "gh_check_state: wrapper subprocess policy files unavailable; no check census was measured" >&2
    exit 3
  fi
  gh_policy_args+=(--sandbox-read-root "$gh_implementation"
    --sandbox-read-root "$GH_BUDGET_REAL_GH"
    --sandbox-write-root "$GH_BUDGET_STATE_DIR"
    --grant 'gh_budget_state=env:GH_BUDGET_STATE_DIR,expose=GH_BUDGET_STATE_DIR,for=gh'
    --grant 'gh_budget_real=env:GH_BUDGET_REAL_GH,expose=GH_BUDGET_REAL_GH,for=gh')
fi

if [[ -z "${GH_TOKEN:-}" ]]; then
  if ! GH_TOKEN="$(gh auth token 2>/dev/null)" || [[ -z "$GH_TOKEN" ]]; then
    echo "gh_check_state: no GH_TOKEN and \`gh auth token\` produced none" >&2
    exit 3
  fi
  export GH_TOKEN
fi

gh_config_dir="$(mktemp -d "${TMPDIR:-/tmp}/gh-check-state-cfg.XXXXXX")"
trap 'rm -rf "$gh_config_dir"' EXIT
export GH_CONFIG_DIR="$gh_config_dir"

run_args=(run \
  --allow-process-network \
  "${gh_policy_args[@]}" \
  --sandbox-read-root "$gh_config_dir" \
  --grant 'gh_token=env:GH_TOKEN,expose=GH_TOKEN,for=gh' \
  --grant 'gh_config=env:GH_CONFIG_DIR,expose=GH_CONFIG_DIR,for=gh')
# Preserve an explicitly supplied Go resolver setting for gh alone. Do not
# inherit the parent environment into arbitrary sandboxed child processes.
if [[ -n "${GODEBUG:-}" ]]; then
  run_args+=(--grant 'gh_dns=env:GODEBUG,expose=GODEBUG,for=gh')
fi

set +e
"$harn_bin" "${run_args[@]}" "$script_dir/gh_check_state.harn" -- "$@"
status=$?
set -e
exit "$status"
