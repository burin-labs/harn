#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
gate_script="$repo_root/.github/scripts/merge-group-path-gate.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

new_repo() {
  local dir="$tmp_root/repo"
  mkdir -p "$dir"
  git -C "$dir" init -q
  git -C "$dir" config user.name "Harn Test"
  git -C "$dir" config user.email "harn-test@example.invalid"
  printf '%s\n' "$dir"
}

commit_all() {
  local dir=$1
  local message=$2
  git -C "$dir" add .
  git -C "$dir" commit -q -m "$message"
}

run_gate() {
  local dir=$1
  local event_name=$2
  local base_sha=$3
  local pattern=$4
  (
    cd "$dir"
    EVENT_NAME="$event_name" \
    BASE_SHA="$base_sha" \
    HEAD_SHA=HEAD \
    PATH_PATTERN="$pattern" \
    GITHUB_OUTPUT= \
    bash "$gate_script"
  )
}

repo=$(new_repo)
mkdir -p "$repo/docs" "$repo/crates/harn-cli/src/commands"
printf 'intro\n' > "$repo/docs/intro.md"
commit_all "$repo" base
base_sha=$(git -C "$repo" rev-parse HEAD)

printf 'intro v2\n' > "$repo/docs/intro.md"
commit_all "$repo" "docs only"
docs_output=$(run_gate "$repo" merge_group "$base_sha" '^crates/harn-cli/src/commands/')
if [ "$docs_output" != "false" ]; then
  echo "expected docs-only merge_group diff to skip, got $docs_output" >&2
  exit 1
fi

printf 'handler\n' > "$repo/crates/harn-cli/src/commands/run.rs"
commit_all "$repo" "handler change"
handler_output=$(run_gate "$repo" merge_group "$base_sha" '^crates/harn-cli/src/commands/')
if [ "$handler_output" != "true" ]; then
  echo "expected relevant merge_group diff to run, got $handler_output" >&2
  exit 1
fi

non_queue_output=$(run_gate "$repo" pull_request "$base_sha" '^no-match/')
if [ "$non_queue_output" != "true" ]; then
  echo "expected non-merge_group event to run, got $non_queue_output" >&2
  exit 1
fi

missing_base_output=$(run_gate "$repo" merge_group 0000000000000000000000000000000000000000 '^no-match/')
if [ "$missing_base_output" != "true" ]; then
  echo "expected missing merge_group base to run defensively, got $missing_base_output" >&2
  exit 1
fi

github_output="$tmp_root/github-output.txt"
(
  cd "$repo"
  GITHUB_OUTPUT="$github_output" \
    OUTPUT_NAME=should_run \
    EVENT_NAME=merge_group \
    BASE_SHA="$base_sha" \
    HEAD_SHA=HEAD \
    PATH_PATTERN='^no-match/' \
    bash "$gate_script"
)
if ! grep -qx 'should_run=false' "$github_output"; then
  echo "expected GitHub output file to carry should_run=false:" >&2
  cat "$github_output" >&2
  exit 1
fi

echo "merge_group_path_gate_test: ok"
