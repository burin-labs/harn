#!/usr/bin/env bash
set -euo pipefail

# Read one head commit's CI check state as a typed census.
#
# The native command grant owns credential confinement. Installed budget owners
# refresh their account index before Harn prepares a retained native invocation.
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
gh_auth_program="$gh_program"
if [[ "$(head -c 2 "$gh_program")" == '#!' ]]; then
  source "$script_dir/lib/sha256.sh"
  owner="$(dirname "$gh_program")/.harn-github-budget-owner.json"
  if [[ ! -f "$owner" ]] || ! jq -e --arg executable "$gh_program" '
      type == "object" and
      (keys == ["account_config", "adapter", "implementation", "interpreter", "native_gh", "schema", "state_directory", "wrapper"]) and
      .schema == "github.budget-owner.v1" and .wrapper.path == $executable and
      ([.account_config, .state_directory] | all(type == "string" and startswith("/") and (explode | all(. >= 32)))) and
      ([.wrapper, .implementation, .adapter, .interpreter, .native_gh] | all(
        type == "object" and keys == ["path", "sha256"] and
        (.path | type == "string" and startswith("/") and (explode | all(. >= 32))) and
        (.sha256 | type == "string" and test("^[0-9a-f]{64}$"))))
    ' "$owner" >/dev/null; then
    echo "gh_check_state: enrolled budget owner unavailable or invalid; no check census was measured" >&2
    exit 3
  fi
  for role in wrapper implementation adapter interpreter native_gh; do
    file="$(jq -r --arg role "$role" '.[$role].path' "$owner")"
    expected="$(jq -r --arg role "$role" '.[$role].sha256' "$owner")"
    if [[ ! -f "$file" || ! -x "$file" ]] || ! actual="$(sha256_file_hex "$file")" \
      || [[ "$actual" != "$expected" ]] || [[ "$(realpath "$file")" != "$file" ]]; then
      echo "gh_check_state: enrolled budget owner identity changed; no check census was measured" >&2
      exit 3
    fi
  done
  interpreter="$(jq -r '.interpreter.path' "$owner")"
  implementation="$(jq -r '.implementation.path' "$owner")"
  gh_auth_program="$(jq -r '.native_gh.path' "$owner")"
  account_config="$(jq -r '.account_config' "$owner")"
  if [[ "$(dirname "$implementation")" != "$(dirname "$gh_program")" \
    || "$(basename "$account_config")" != hosts.yml ]] \
    || ! env -u GH_TOKEN -u GITHUB_TOKEN GH_DEBUG= GH_BUDGET_PRINT_REAL= \
      GH_BUDGET_REAL_GH="$gh_auth_program" \
      GH_BUDGET_STATE_DIR="$(jq -r '.state_directory' "$owner")" \
      GH_CONFIG_DIR="$(dirname "$account_config")" \
      "$interpreter" -I "$implementation" --budget-refresh-account-index; then
    echo "gh_check_state: enrolled account refresh failed; no check census was measured" >&2
    exit 3
  fi
fi

if [[ -z "${GH_TOKEN:-}" ]]; then
  if ! GH_TOKEN="$("$gh_auth_program" auth token 2>/dev/null)" || [[ -z "$GH_TOKEN" ]]; then
    echo "gh_check_state: no GH_TOKEN and \`gh auth token\` produced none" >&2
    exit 3
  fi
  export GH_TOKEN
fi

run_args=(run \
  --allow-process-network \
  "${gh_policy_args[@]}" \
  --grant 'gh_token=env:GH_TOKEN,expose=GH_TOKEN,for=gh')

set +e
"$harn_bin" "${run_args[@]}" "$script_dir/gh_check_state.harn" -- "$@"
status=$?
set -e
exit "$status"
