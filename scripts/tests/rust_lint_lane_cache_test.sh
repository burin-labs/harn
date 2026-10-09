#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

for leg in workspace lean-lsp freshness-checker all; do
fixture="$tmp_root/$leg"
mkdir -p "$fixture/src"
package=harn-lsp
if [[ "$leg" == freshness-checker ]]; then package=harn-cli; fi
cat > "$fixture/Cargo.toml" <<TOML
[package]
name = "$package"
version = "0.1.0"
edition = "2021"

[workspace]

[features]
internal-freshness-checker = []

[[bin]]
name = "harn-freshness-check"
path = "src/main.rs"
required-features = ["internal-freshness-checker"]
TOML
printf 'fn main() {}\n' > "$fixture/src/main.rs"
cat > "$fixture/src/lib.rs" <<'RS'
pub enum Node {
    NilLiteral,
    Other,
}

pub fn has_tools(call_name: &str, tools: Option<&Node>) -> bool {
    call_name.starts_with("llm_")
        && tools.is_none_or(|node| matches!(node, Node::NilLiteral))
}
RS

# Seed the exact graph the independent leg will consume.
clippy_args=(--workspace --all-targets)
case "$leg" in
  lean-lsp) clippy_args=(-p harn-lsp) ;;
  freshness-checker) clippy_args=(-p harn-cli --bin harn-freshness-check --features internal-freshness-checker) ;;
esac
(
  cd "$fixture"
  cargo clippy "${clippy_args[@]}" -- -D warnings
)

# Replace the source with the exact lint shape that escaped CI, then model a
# restored target whose artifact timestamp is newer than the checkout source.
cat > "$fixture/src/lib.rs" <<'RS'
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
touch -t 202001010000 "$fixture/src/lib.rs"

# This is the falsifier: without invalidation Cargo reports success without
# invoking Clippy on the changed source.
(
  cd "$fixture"
  cargo clippy "${clippy_args[@]}" -- -D warnings
)

set +e
output="$(
  cd "$fixture"
  "$repo_root/scripts/ci/run_rust_lint_lane.sh" --leg "$leg" 2>&1
)"
lint_status=$?
set -e

if [[ "$lint_status" -eq 0 ]]; then
  echo "cache-safe lint leg $leg did not recompile changed source" >&2
  exit 1
fi
if [[ "$output" != *"strict Clippy reached changed source"* ]]; then
  echo "cache-safe lint leg $leg did not reach the changed source" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi
echo "cache-safe lint leg $leg reached changed source"
done

echo "rust_lint_lane_cache_test: ok"
