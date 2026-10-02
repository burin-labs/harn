#!/usr/bin/env bash
# Verify the exact registry-driven filter against nextest's archived test inventory.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
inventory="${1:?usage: verify_host_bound_rust_archive.sh <nextest-inventory.json>}"
selected_file="$(mktemp "${TMPDIR:-/tmp}/host-bound-rust-selected.XXXXXX")"
trap 'rm -f "$selected_file"' EXIT

scripts_filter="$SCRIPT_DIR/host_bound_rust_test_filter.sh"
"$scripts_filter" linux >/dev/null
expected_names="$("$scripts_filter" linux names)"

if ! jq -e '
  .["rust-suites"] as $suites
  | ($suites | type == "object" and length > 0)
    and all($suites[];
      # Values mirror nextest_metadata::RustTestSuiteStatusSummary in the
      # pinned cargo-nextest 0.9.x line. Unknown states must fail closed.
      ((.status == "listed") or (.status == "skipped") or (.status == "skipped-default-filter"))
      and (."package-name" | type == "string")
      and (."binary-name" | type == "string")
      and ((.status != "listed") or (.testcases | type == "object"))
      and ((.status == "listed") or ((.testcases // {}) | type == "object" and length == 0))
    )
    and all([
      $suites[] | select(.status == "listed") | .testcases | to_entries[]
      | .value["filter-match"].status
    ][]; . == "matches" or . == "mismatch")
  ' "$inventory" >/dev/null; then
  echo "error: nextest archive inventory contains invalid or incomplete Rust suites" >&2
  exit 1
fi

jq -er '
  .["rust-suites"] as $suites
  | [
      $suites | to_entries[] as $entry
      | $entry.value as $suite
      | select($suite.status == "listed")
      | $suite.testcases | to_entries[]
      | select(.value["filter-match"].status == "matches")
      | "\($suite["package-name"])::\($suite["binary-name"])$\(.key)"
    ]
  | if length == 0 then error("filter selected no tests") else .[] end
' "$inventory" > "$selected_file" || {
  echo "error: nextest archive inventory selected no host-bound tests" >&2
  exit 1
}

selected_count="$(wc -l < "$selected_file" | tr -d ' ')"
expected_count=0
while IFS= read -r expected || [[ -n "$expected" ]]; do
  [[ -n "$expected" ]] || continue
  matches="$(jq -r --arg expected "$expected" '
    [
      .["rust-suites"] | to_entries[] | .value as $suite
      | select($suite.status == "listed")
      | $suite.testcases | to_entries[]
      | select(.value["filter-match"].status == "matches")
      # Match exact Rust name components. A registry entry may name either a
      # test function or a test module containing several host-bound cases.
      | select((.key | split("::") | index($expected)) != null)
    ] | length
  ' "$inventory")"
  if (( matches == 0 )); then
    echo "error: host-bound registry entry is absent from archived tests: $expected" >&2
    exit 1
  fi
  ((expected_count += 1))
done <<< "$expected_names"

while IFS= read -r selected; do
  test_name="${selected#*$}"
  attributed=0
  while IFS= read -r expected || [[ -n "$expected" ]]; do
    [[ -n "$expected" ]] || continue
    if [[ "::$test_name::" == *"::$expected::"* ]]; then
      attributed=1
      break
    fi
  done <<< "$expected_names"
  if (( attributed == 0 )); then
    echo "error: archived filter selected a test outside the host-bound registry: $selected" >&2
    exit 1
  fi
done < "$selected_file"

printf 'host_bound_archive_inventory selected=%s registry=%s\n' "$selected_count" "$expected_count"
