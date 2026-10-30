#!/usr/bin/env bash
# Prove the real archive -> source reconciliation -> Cargo -> executable path.
# This needs Rust and belongs in a build lane, not the source-only shell gate.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/reuse_workspace_crates.sh"
root=$(mktemp -d)
cleanup() {
  local status=$?
  if ((status != 0)) && [[ -f "$root/cargo-build.log" ]]; then
    cat "$root/cargo-build.log" >&2
    [[ ! -f "$root/cargo-artifacts.jsonl" ]] || cat "$root/cargo-artifacts.jsonl" >&2
  fi
  rm -rf "$root"
}
trap cleanup EXIT
mkdir -p "$root/fixture/crates/dep/src" "$root/fixture/src"
cd "$root/fixture"
git init -q -b main .
git config user.email test@example.com
git config user.name test
git config commit.gpgsign false
printf 'target/\n.harn-workspace-source/\n' > .gitignore
cp "$repo_root/rust-toolchain.toml" .
cat > Cargo.toml <<'TOML'
[package]
name = "reuse_fixture_app"
version = "0.0.0"
edition = "2021"
[dependencies]
reuse_fixture_dep = { path = "crates/dep" }
TOML
cat > crates/dep/Cargo.toml <<'TOML'
[package]
name = "reuse_fixture_dep"
version = "0.0.0"
edition = "2021"
TOML
printf 'pub fn value() -> u32 { 1 }\n' > crates/dep/src/lib.rs
printf 'fn main() { println!("{}", reuse_fixture_dep::value()); }\n' > src/main.rs
build_fixture() {
  env CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_TARGET_DIR="$PWD/target" \
    CARGO_TERM_COLOR=always \
    CARGO_LOG=cargo::core::compiler::fingerprint=trace \
    cargo build --offline --verbose --message-format=json \
      > "$root/cargo-artifacts.jsonl" 2> "$root/cargo-build.log"
}
assert_freshness() {
  jq -e -s --argjson expected "$1" '
    [.[] | select(.reason == "compiler-artifact")] as $artifacts |
    ($artifacts | map(.target.name) | sort) == ["reuse_fixture_app", "reuse_fixture_dep"]
    and all($artifacts[]; .fresh == $expected)
  ' "$root/cargo-artifacts.jsonl" >/dev/null
}
build_fixture
assert_freshness false
[[ "$(target/debug/reuse_fixture_app)" == 1 ]]
git add -A && git commit -q -m compiled
env CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 \
  "$repo_root/scripts/ci/workspace_warm_artifact.sh" macos pack "$root/warm-bundle"
touch src/main.rs crates/dep/src/lib.rs
env CARGO_INCREMENTAL=0 "$repo_root/scripts/ci/workspace_warm_artifact.sh" \
  macos restore "$root/warm-bundle" "$PWD/target"
"$script" restore
build_fixture
assert_freshness true
"$script" record
printf 'pub fn value() -> u32 { 2 }\n' > crates/dep/src/lib.rs
git add crates/dep/src/lib.rs && git commit -q -m changed-dependency
"$script" restore
build_fixture
assert_freshness false
[[ "$(target/debug/reuse_fixture_app)" == 2 ]]
echo "workspace_reuse_cargo_test: unchanged units reused; changed dependency and executable rebuilt with new output"
