#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

mkdir -p "$tmp_root/src"
cat > "$tmp_root/Cargo.toml" <<'TOML'
[package]
name = "rust-lint-lane-cache-test"
version = "0.1.0"
edition = "2021"

[workspace]
TOML
cat > "$tmp_root/src/lib.rs" <<'RS'
pub enum Node {
    NilLiteral,
    Other,
}

pub fn has_tools(call_name: &str, tools: Option<&Node>) -> bool {
    call_name.starts_with("llm_")
        && tools.is_none_or(|node| matches!(node, Node::NilLiteral))
}
RS

# Seed a warning-clean strict-Clippy workspace artifact.
(
  cd "$tmp_root"
  cargo clippy -- -D warnings
)

# Replace the source with the exact lint shape that escaped CI, then model a
# restored target whose artifact timestamp is newer than the checkout source.
cat > "$tmp_root/src/lib.rs" <<'RS'
pub enum Node {
    NilLiteral,
    Other,
}

compile_error!("strict Clippy reached changed source");

pub fn has_tools(call_name: &str, tools: Option<&Node>) -> bool {
    call_name.starts_with("llm_")
        && !tools.is_some_and(|node| !matches!(node, Node::NilLiteral))
}
RS
touch -t 202001010000 "$tmp_root/src/lib.rs"

# This is the falsifier: without invalidation Cargo reports success without
# invoking Clippy on the changed source.
(
  cd "$tmp_root"
  cargo clippy -- -D warnings
)

set +e
output="$(
  cd "$tmp_root"
  RUN_PROMPT_PROSE_RATCHET=false "$repo_root/scripts/ci/run_rust_lint_lane.sh" 2>&1
)"
lint_status=$?
set -e

if [[ "$lint_status" -eq 0 ]]; then
  echo "cache-safe lint entrypoint did not recompile changed source" >&2
  exit 1
fi
if [[ "$output" != *"strict Clippy reached changed source"* ]]; then
  echo "cache-safe lint entrypoint did not reach the changed source" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi

echo "rust_lint_lane_cache_test: ok"
