#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
owner=${1:-$repo_root/scripts/ci/sprint_fast_ci.sh}
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

check() {
  local flag=$1 event=$2 expected=$3
  printf 'known_non_null=present\n' > "$scratch/output"
  SPRINT_FAST_CI="$flag" EVENT_NAME="$event" GITHUB_OUTPUT="$scratch/output" \
    bash "$owner" > "$scratch/stdout"
  local actual
  actual=$(cat "$scratch/output")
  if [[ "$actual" != $'known_non_null=present\nactive='"$expected" ]]; then
    printf 'wrong sprint decision: flag=%s event=%s expected=%s\n' "$flag" "$event" "$expected" >&2
    exit 1
  fi
}

check true pull_request true
check true merge_group true
for event in push schedule workflow_dispatch pull_request_target; do
  check true "$event" false
done
for flag in '' false True 1; do
  check "$flag" pull_request false
  check "$flag" merge_group false
done

if env -u EVENT_NAME GITHUB_OUTPUT="$scratch/output" bash "$owner" > "$scratch/stdout" 2>&1; then
  echo 'missing event was accepted' >&2
  exit 1
fi

# The original fragment guard accepted this mutation, which overrides main.
if [[ $# == 0 ]]; then
  sed 's/^active=false$/active=true/' "$owner" > "$scratch/overridden.sh"
  if bash "${BASH_SOURCE[0]}" "$scratch/overridden.sh" > "$scratch/negative.log" 2>&1; then
    echo 'main override escaped the owning shell controls' >&2
    exit 1
  fi
  if ! grep -q 'flag=true event=push expected=false' "$scratch/negative.log"; then
    echo 'negative control did not reach the main push decision' >&2
    exit 1
  fi
  echo 'sprint decision: 14 event/value controls passed; main override refused'
fi
