#!/usr/bin/env bash
set -euo pipefail

# Resolve whether a reporting-only workflow should run on a merge_group ref.
# Non-merge-group events always run. Merge-group events run only when the
# speculative diff touches PATH_PATTERN. If the diff cannot be computed, run
# defensively rather than silently skipping coverage.

output_name="${OUTPUT_NAME:-run_gate}"
event_name="${EVENT_NAME:-${GITHUB_EVENT_NAME:-}}"
base_sha="${BASE_SHA:-}"
head_sha="${HEAD_SHA:-HEAD}"
path_pattern="${PATH_PATTERN:-}"
gate_name="${GATE_NAME:-merge-group path gate}"

write_output() {
  local value="$1"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf '%s=%s\n' "$output_name" "$value" >> "$GITHUB_OUTPUT"
  else
    printf '%s\n' "$value"
  fi
}

if [ -z "$path_pattern" ]; then
  echo "::error::PATH_PATTERN is required for $gate_name." >&2
  exit 2
fi

if [ "$event_name" != "merge_group" ]; then
  echo "$gate_name: $event_name event; running gate." >&2
  write_output true
  exit 0
fi

if [ -z "$base_sha" ] || [ "$base_sha" = "0000000000000000000000000000000000000000" ]; then
  echo "$gate_name: missing merge-group base SHA; running defensively." >&2
  write_output true
  exit 0
fi

if ! git cat-file -e "${base_sha}^{commit}" 2>/dev/null; then
  git fetch --no-tags --depth=1 origin "$base_sha" >/dev/null 2>&1 || true
fi

changed_files="$(mktemp)"
trap 'rm -f "$changed_files"' EXIT

if ! git diff --name-only "$base_sha" "$head_sha" > "$changed_files" 2>/dev/null; then
  echo "$gate_name: could not diff $base_sha..$head_sha; running defensively." >&2
  write_output true
  exit 0
fi

echo "$gate_name: merge-group changed files:" >&2
sed 's/^/  /' "$changed_files" >&2

if grep -E -q "$path_pattern" "$changed_files"; then
  echo "$gate_name: relevant merge-group diff; running gate." >&2
  write_output true
else
  echo "$gate_name: irrelevant merge-group diff; skipping gate." >&2
  write_output false
fi
