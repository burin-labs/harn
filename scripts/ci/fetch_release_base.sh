#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 || ! "$2" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: release base requires a branch name and captured commit SHA" >&2
  exit 1
fi
base_ref=$1
base_sha=$2
git check-ref-format "refs/remotes/origin/$base_ref"

# The local name is the release checker's existing interface. Its contents
# come only from the captured commit, never a later remote branch tip.
git fetch --no-tags origin "+${base_sha}:refs/remotes/origin/${base_ref}"
