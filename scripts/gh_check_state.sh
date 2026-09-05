#!/usr/bin/env bash
set -euo pipefail

# Read one head commit's CI check state as a typed census.
#
# The Harn entry point runs inside the default worktree sandbox, so `gh` needs
# two things granted explicitly: an API token, and a config directory it is
# allowed to read. Granting them here keeps the run sandboxed instead of
# reaching for --no-sandbox.
#
# usage: scripts/gh_check_state.sh --repo OWNER/NAME --sha <40-hex> [--base REF]
#                                  [--workflow PATH] [--expect NAME ...] [--json]
#
# Exit codes: 0 green, 1 failing, 2 pending, 3 missing or unobservable,
# 64 usage error.

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

set +e
"$script_dir/harn_bin.sh" run \
  --allow-process-network \
  --sandbox-read-root "$gh_config_dir" \
  --grant gh_token=env:GH_TOKEN,expose=GH_TOKEN,for=gh \
  --grant gh_config=env:GH_CONFIG_DIR,expose=GH_CONFIG_DIR,for=gh \
  "$script_dir/gh_check_state.harn" -- "$@"
status=$?
set -e
exit "$status"
