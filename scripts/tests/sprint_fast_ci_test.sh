#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
policy="$repo_root/scripts/ci_sprint_fast_ci.sh"
checker="$repo_root/scripts/check_sprint_fast_ci.mjs"

expect() {
  expected=$1
  shift
  actual=$("$policy" resolve "$@")
  [ "$actual" = "$expected" ] \
    || { echo "resolve $* produced $actual, expected $expected" >&2; exit 1; }
}

expect true "" pull_request refs/pull/1/merge
expect true false merge_group refs/heads/gh-readonly-queue/main/pr-1
expect true typo pull_request refs/pull/1/merge
expect false true pull_request refs/pull/1/merge
expect false true merge_group refs/heads/gh-readonly-queue/main/pr-1
expect false true push refs/heads/topic
expect true true push refs/heads/main

verify_output=$("$policy" verify false \
  "Linux sandbox tests=skipped" \
  "Harn proof workers=skipped" \
  "Windows cross-compile check=skipped" \
  "Rust on macOS=skipped")
[[ "$verify_output" == *"pending=0 failing=0"* ]] \
  || { echo "deferred census did not expose measured zeroes" >&2; exit 1; }

if "$policy" verify false \
  "Linux sandbox tests=failure" \
  "Harn proof workers=skipped" \
  "Windows cross-compile check=skipped" \
  "Rust on macOS=skipped" >/dev/null; then
  echo "deferred verification accepted a failed slow check" >&2
  exit 1
fi
if "$policy" verify false \
  "Linux sandbox tests=" \
  "Harn proof workers=skipped" \
  "Windows cross-compile check=skipped" \
  "Rust on macOS=skipped" >/dev/null; then
  echo "deferred verification accepted a missing slow-check reading" >&2
  exit 1
fi
if "$policy" verify false \
  "Linux sandbox tests=skipped" \
  "Harn proof workers=skipped" \
  "Windows cross-compile check=skipped" >/dev/null 2>&1; then
  echo "deferred verification accepted an absent required reading" >&2
  exit 1
fi

node "$checker" "$repo_root/.github/workflows/ci.yml"

fixture_root=$(mktemp -d)
trap 'rm -rf "$fixture_root"' EXIT
fixture="$fixture_root/ci.yml"
cp "$repo_root/.github/workflows/ci.yml" "$fixture"
perl -0pi -e "s/needs\.changes\.outputs\.run_slow_ci == 'true'\n      && needs\.changes\.outputs\.rust == 'true'/needs.changes.outputs.rust == 'true'/" "$fixture"
if node "$checker" "$fixture" >/dev/null 2>&1; then
  echo "workflow checker accepted a slow job with no sprint gate" >&2
  exit 1
fi

echo "sprint_fast_ci_test: ok"
