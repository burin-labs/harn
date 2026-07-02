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
  actual="$(./scripts/ci_release_metadata_only.sh "$paths_file")"
  if [ "$actual" != "$expected" ]; then
    echo "expected $expected, got $actual for:" >&2
    sed 's/^/  /' "$paths_file" >&2
    exit 1
  fi
}

check true \
  CHANGELOG.md \
  Cargo.lock \
  Cargo.toml \
  changelog.d/provider-wire-correctness.fixed.md \
  conformance/protocols/fixtures/a2a/agent_card.valid.json \
  docs/src/embedding-rust.md \
  spec/acp-registry/harn/agent.json \
  spec/protocol-artifacts/HarnProtocol.swift \
  spec/protocol-artifacts/go/harnprotocol/harnprotocol.go

check false
check false crates/harn-vm/src/lib.rs
check false scripts/release_ship.sh
check false crates/harn-cli/portal/src/main.tsx

echo "ci_release_metadata_only_test: ok"
