#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
event="${1:?event path is required}"
repo="${2:?repository is required}"
number="${3:?pull request number is required}"
event_opted_out="$(jq -r -L "$script_dir" \
  'include "review_dispatch_opt_out"; review_dispatch_event_opted_out' "$event")"
if [[ "$event_opted_out" == true ]]; then
  printf 'true\n'
else
  bash "$script_dir/review_dispatch_labels.sh" "$repo" "$number"
fi
