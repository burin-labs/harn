#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo="${1:?repository is required}"
number="${2:?pull request number is required}"
[[ "$repo" =~ ^[^/[:space:]]+/[^/[:space:]]+$ && "$number" =~ ^[1-9][0-9]*$ ]] \
  || { echo "invalid review label lookup" >&2; exit 2; }

pages="$(gh api "repos/$repo/issues/$number/labels?per_page=100" --paginate --slurp)" \
  || { echo "could not read review labels for $repo#$number" >&2; exit 2; }
jq -r -L "$script_dir" 'include "review_dispatch_opt_out"; review_dispatch_pages_opted_out' <<<"$pages"
