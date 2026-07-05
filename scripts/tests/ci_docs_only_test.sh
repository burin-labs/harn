#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

check() {
  local expected="$1"
  shift
  local paths_file="$tmp_dir/paths.txt"
  printf '%s\n' "$@" > "$paths_file"
  local actual
  actual="$(./scripts/ci_docs_only.sh "$paths_file")"
  if [ "$actual" != "$expected" ]; then
    echo "expected $expected, got $actual for:" >&2
    sed 's/^/  /' "$paths_file" >&2
    exit 1
  fi
}

check true \
  README.md \
  docs/src/SUMMARY.md \
  website/src/App.tsx \
  website/package-lock.json \
  changelog.d/4115.fixed.md

check true ./docs/src/stdlib/governors.md
check false
check false crates/harn-vm/src/lib.rs
check false scripts/release_ship.sh
check false .github/workflows/ci.yml
check false Cargo.toml

echo "ci_docs_only_test: ok"
