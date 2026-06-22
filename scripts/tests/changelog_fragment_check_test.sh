#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
gate_script="$repo_root/.github/scripts/changelog-fragment-check.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

new_repo() {
  local name=$1
  local dir="$tmp_root/$name"
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
  local base=$2
  (cd "$dir" && BASE_SHA="$base" HEAD_SHA=HEAD bash "$gate_script")
}

expect_pass() {
  local dir=$1
  local base=$2
  local output
  output=$(run_gate "$dir" "$base" 2>&1)
  printf '%s\n' "$output"
}

expect_fail() {
  local dir=$1
  local base=$2
  local output
  if output=$(run_gate "$dir" "$base" 2>&1); then
    printf 'expected changelog gate to fail, but it passed:\n%s\n' "$output" >&2
    exit 1
  fi
  printf '%s\n' "$output"
}

cargo_repo=$(new_repo cargo)
cat > "$cargo_repo/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.1.0"

[dependencies]
serde = "1.0.0"
EOF
printf '# lock\n' > "$cargo_repo/Cargo.lock"
commit_all "$cargo_repo" base
cargo_base=$(git -C "$cargo_repo" rev-parse HEAD)
perl -0pi -e 's/serde = "1\.0\.0"/serde = "1.0.1"/' "$cargo_repo/Cargo.toml"
printf '# lock\nserde 1.0.1\n' > "$cargo_repo/Cargo.lock"
commit_all "$cargo_repo" "bump cargo dependency"
cargo_output=$(expect_pass "$cargo_repo" "$cargo_base")
grep -q "only dependency manifest/lockfile paths touched" <<<"$cargo_output"

npm_repo=$(new_repo npm)
mkdir -p "$npm_repo/crates/harn-cli/portal"
printf '{"dependencies":{"left-pad":"1.0.0"}}\n' > "$npm_repo/crates/harn-cli/portal/package.json"
printf 'lockfileVersion: 9\n' > "$npm_repo/crates/harn-cli/portal/pnpm-lock.yaml"
commit_all "$npm_repo" base
npm_base=$(git -C "$npm_repo" rev-parse HEAD)
printf '{"dependencies":{"left-pad":"1.0.1"}}\n' > "$npm_repo/crates/harn-cli/portal/package.json"
printf 'lockfileVersion: 9\npackages: {}\n' > "$npm_repo/crates/harn-cli/portal/pnpm-lock.yaml"
commit_all "$npm_repo" "bump npm dependency"
npm_output=$(expect_pass "$npm_repo" "$npm_base")
grep -q "only dependency manifest/lockfile paths touched" <<<"$npm_output"

mixed_repo=$(new_repo mixed)
mkdir -p "$mixed_repo/crates/harn-vm/src"
printf 'pub fn value() -> u8 { 1 }\n' > "$mixed_repo/crates/harn-vm/src/lib.rs"
printf '# lock\n' > "$mixed_repo/Cargo.lock"
commit_all "$mixed_repo" base
mixed_base=$(git -C "$mixed_repo" rev-parse HEAD)
printf 'pub fn value() -> u8 { 2 }\n' > "$mixed_repo/crates/harn-vm/src/lib.rs"
printf '# lock\nserde 1.0.1\n' > "$mixed_repo/Cargo.lock"
commit_all "$mixed_repo" "change source with dependency"
mixed_output=$(expect_fail "$mixed_repo" "$mixed_base")
grep -q "crates/harn-vm/src/lib.rs" <<<"$mixed_output"

echo "changelog_fragment_check_test: ok"
