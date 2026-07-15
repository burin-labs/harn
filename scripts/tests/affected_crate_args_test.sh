#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/affected_crate_args.sh"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

fake_repo="$tmpdir/fake-repo"
mkdir -p "$fake_repo/crates/harn-core/src" \
  "$fake_repo/crates/harn-cli/src" \
  "$fake_repo/crates/harn-tools/src" \
  "$fake_repo/docs"
cd "$fake_repo"
git init -q
git config user.email test@example.com
git config user.name "Test User"
cat > Cargo.toml <<'TOML'
[workspace]
members = ["crates/*"]
resolver = "2"
TOML
cat > crates/harn-core/Cargo.toml <<'TOML'
[package]
name = "harn-core"
version = "0.0.0"
edition = "2021"
TOML
cat > crates/harn-core/src/lib.rs <<'RS'
pub fn core() {}
RS
cat > crates/harn-cli/Cargo.toml <<'TOML'
[package]
name = "harn-cli"
version = "0.0.0"
edition = "2021"

[dependencies]
harn-core = { path = "../harn-core" }
TOML
cat > crates/harn-cli/src/lib.rs <<'RS'
pub fn cli() {}
RS
cat > crates/harn-tools/Cargo.toml <<'TOML'
[package]
name = "harn-tools"
version = "0.0.0"
edition = "2021"

[dependencies]
harn-core = { path = "../harn-core" }
TOML
cat > crates/harn-tools/src/lib.rs <<'RS'
pub fn tools() {}
RS
git add .
git commit -qm base
base_commit="$(git rev-parse HEAD)"

global_changes="$tmpdir/global.txt"
cat > "$global_changes" <<'EOF'
.github/workflows/ci.yml
crates/harn-vm/src/lib.rs
EOF

if ! output=$(HARN_BIN="$tmpdir/missing-harn" "$script" --changed-files-file "$global_changes" 2>"$tmpdir/global.err"); then
  cat "$tmpdir/global.err" >&2
  exit 1
fi
if [[ "$output" != "--workspace" ]]; then
  echo "expected global path fast path to select --workspace, got: $output" >&2
  exit 1
fi
grep -q "global/workspace-level change detected" "$tmpdir/global.err"

no_changes="$tmpdir/empty.txt"
: > "$no_changes"
if ! output=$(HARN_BIN="$tmpdir/missing-harn" "$script" --changed-files-file "$no_changes" 2>"$tmpdir/empty.err"); then
  cat "$tmpdir/empty.err" >&2
  exit 1
fi
if [[ -n "$output" ]]; then
  echo "expected empty changed-file set to select nothing, got: $output" >&2
  exit 1
fi
grep -q "no files changed" "$tmpdir/empty.err"

run_case() {
  local name="$1"
  local expected="$2"
  shift 2

  git checkout -qB "$name" "$base_commit"
  "$@"
  git add .
  git commit -qm "$name"
  local actual
  actual="$("$script" --base "$base_commit" 2>"$tmpdir/$name.err")"
  if [[ "$actual" != "$expected" ]]; then
    echo "case $name expected args: $expected" >&2
    echo "case $name actual args:   $actual" >&2
    echo "--- stderr ---" >&2
    cat "$tmpdir/$name.err" >&2
    exit 1
  fi
}

run_case core-rdeps "--workspace" bash -c '
  printf "\npub fn changed() {}\n" >> crates/harn-core/src/lib.rs
'
grep -q "selected (changed + rdeps closure): harn-cli harn-core harn-tools" "$tmpdir/core-rdeps.err"

run_case cli-only "-p harn-cli" bash -c '
  printf "\npub fn changed() {}\n" >> crates/harn-cli/src/lib.rs
'
grep -q "pruned (not selected): harn-core harn-tools" "$tmpdir/cli-only.err"

run_case docs-only "" bash -c '
  printf "hello\n" > docs/readme.md
'
grep -q "changed files touch no Rust crate" "$tmpdir/docs-only.err"

run_case global-from-diff "--workspace" bash -c '
  printf "\n# workspace comment\n" >> Cargo.toml
'
grep -q "selecting the FULL workspace" "$tmpdir/global-from-diff.err"

if "$script" --base "$base_commit" --unexpected >"$tmpdir/unexpected.out" 2>"$tmpdir/unexpected.err"; then
  echo "expected unexpected argument to fail" >&2
  exit 1
fi
grep -q "unexpected argument" "$tmpdir/unexpected.err"

echo "affected_crate_args_test: ok"
