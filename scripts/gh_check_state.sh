#!/usr/bin/env bash
set -euo pipefail

# Read one head commit's CI check state as a typed census.
#
# The Harn entry point runs inside the default worktree sandbox. Grant gh its
# token and native executable. The protected process owner creates the private
# CLI configuration; the launcher keeps credentials out of other commands.
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
if [[ "$(head -c 2 "$gh_program")" == '#!' ]]; then
  echo "gh_check_state: scripted GitHub CLI wrappers cannot receive protected command grants; no check census was measured" >&2
  exit 3
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

run_args=(run --environment-policy granted \
  --sandbox-read-root "$gh_program" \
  --sandbox-read-root "$gh_config_dir" \
  --grant 'gh_token=env:GH_TOKEN,expose=GH_TOKEN,for=gh')
# Prove the guard with a canary, before the real credential or an API request
# reaches Harn. The probe makes no network request and never prints a token.
if ! GH_TOKEN=gh-check-state-contract-canary "$harn_bin" "${run_args[@]}" \
  "$script_dir/gh_check_state_contract.harn" >"$gh_config_dir/contract.json" 2>"$gh_config_dir/contract.err" \
  || ! jq -e 'type == "object" and .contract == "harn.protected-command-grants.v1"' \
    "$gh_config_dir/contract.json" >/dev/null; then
  echo "gh_check_state: Harn cannot prove protected foreground command grants; no check census was measured" >&2
  exit 3
fi

set +e
"$harn_bin" "${run_args[@]}" --allow-process-network "$script_dir/gh_check_state.harn" -- "$@"
status=$?
set -e
exit "$status"
