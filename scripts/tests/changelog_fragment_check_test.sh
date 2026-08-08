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

# A nested, non-dependency file whose name merely starts with "requirements"
# must NOT be mistaken for a pip requirements manifest. The dependency-metadata
# allowlist previously used `requirements.*\.txt`, where `.*` crossed `/`, so a
# path like `.../requirements_helpers/seed.txt` matched and silently bypassed the
# gate. It must require a changelog fragment like any other source change.
reqlike_repo=$(new_repo reqlike)
mkdir -p "$reqlike_repo/crates/harn-vm/src/requirements_helpers"
printf 'seed\n' > "$reqlike_repo/crates/harn-vm/src/requirements_helpers/seed.txt"
commit_all "$reqlike_repo" base
reqlike_base=$(git -C "$reqlike_repo" rev-parse HEAD)
printf 'seed v2\n' > "$reqlike_repo/crates/harn-vm/src/requirements_helpers/seed.txt"
commit_all "$reqlike_repo" "edit a requirements-prefixed source file"
reqlike_output=$(expect_fail "$reqlike_repo" "$reqlike_base")
grep -q "crates/harn-vm/src/requirements_helpers/seed.txt" <<<"$reqlike_output"

# A genuine pip requirements file (single path segment) still counts as
# dependency metadata and passes without a fragment.
req_repo=$(new_repo requirements)
printf 'flask==2.0.0\n' > "$req_repo/requirements-dev.txt"
commit_all "$req_repo" base
req_base=$(git -C "$req_repo" rev-parse HEAD)
printf 'flask==2.0.1\n' > "$req_repo/requirements-dev.txt"
commit_all "$req_repo" "bump python dependency"
req_output=$(expect_pass "$req_repo" "$req_base")
grep -q "only dependency manifest/lockfile paths touched" <<<"$req_output"

# A test-only change under a crate's nested `tests/` directory must pass
# without a fragment. The ignore pattern used to root-anchor every
# alternative, so `crates/*/tests/` never matched `tests?/` and every
# crate-level test-only PR was forced to carry a `no-changelog-needed` label.
nested_tests_repo=$(new_repo nested_tests)
mkdir -p "$nested_tests_repo/crates/harn-hostlib/tests"
printf 'fn t() {}\n' > "$nested_tests_repo/crates/harn-hostlib/tests/proc_e2e.rs"
commit_all "$nested_tests_repo" base
nested_tests_base=$(git -C "$nested_tests_repo" rev-parse HEAD)
printf 'fn t() { assert!(true); }\n' > "$nested_tests_repo/crates/harn-hostlib/tests/proc_e2e.rs"
commit_all "$nested_tests_repo" "edit a nested crate test"
nested_tests_output=$(expect_pass "$nested_tests_repo" "$nested_tests_base")
grep -q "only docs/test/CI paths touched" <<<"$nested_tests_output"

# Documentation/agent files are documentation wherever they live: a nested
# `AGENTS.md` (e.g. `scripts/AGENTS.md`) and a nested `docs/` file must pass
# without a fragment, not just their repo-root counterparts.
nested_docs_repo=$(new_repo nested_docs)
mkdir -p "$nested_docs_repo/scripts" "$nested_docs_repo/crates/harn-cli/docs"
printf 'guidance\n' > "$nested_docs_repo/scripts/AGENTS.md"
printf 'notes\n' > "$nested_docs_repo/crates/harn-cli/docs/design.md"
commit_all "$nested_docs_repo" base
nested_docs_base=$(git -C "$nested_docs_repo" rev-parse HEAD)
printf 'updated guidance\n' > "$nested_docs_repo/scripts/AGENTS.md"
printf 'updated notes\n' > "$nested_docs_repo/crates/harn-cli/docs/design.md"
commit_all "$nested_docs_repo" "edit nested docs and AGENTS.md"
nested_docs_output=$(expect_pass "$nested_docs_repo" "$nested_docs_base")
grep -q "only docs/test/CI paths touched" <<<"$nested_docs_output"

# Guard against over-broadening: a source file whose path segment merely
# ends with an ignorable directory name (e.g. `contests/`) must still require
# a fragment. The `(^|/)` segment guard keeps `tests?/` from matching inside
# `contests/`.
contests_repo=$(new_repo contests)
mkdir -p "$contests_repo/crates/harn-vm/src/contests"
printf 'pub fn v() -> u8 { 1 }\n' > "$contests_repo/crates/harn-vm/src/contests/mod.rs"
commit_all "$contests_repo" base
contests_base=$(git -C "$contests_repo" rev-parse HEAD)
printf 'pub fn v() -> u8 { 2 }\n' > "$contests_repo/crates/harn-vm/src/contests/mod.rs"
commit_all "$contests_repo" "edit a contests-prefixed source file"
contests_output=$(expect_fail "$contests_repo" "$contests_base")
grep -q "crates/harn-vm/src/contests/mod.rs" <<<"$contests_output"

echo "changelog_fragment_check_test: ok"
