#!/usr/bin/env bash
# The breaking-surface gate refuses an undeclared break and accepts a
# declared one. Each case runs the gate in a throwaway repository.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
gate_script="$repo_root/.github/scripts/breaking-surface-check.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

new_repo() {
  local dir="$tmp_root/$1"
  mkdir -p "$dir/spec" "$dir/changelog.d"
  git -C "$dir" init -b main -q
  git -C "$dir" config user.name "Harn Test"
  git -C "$dir" config user.email "harn-test@example.invalid"
  git -C "$dir" config commit.gpgsign false
  printf '%s\n' "$dir"
}

commit_all() {
  git -C "$1" add -A
  git -C "$1" commit -q -m "$2"
}

# run_gate DIR BASE EXPECT(pass|fail) ARGS...
run_gate() {
  local dir=$1 base=$2 expect=$3 output status
  shift 3
  set +e
  output=$(cd "$dir" && BASE_SHA="$base" HEAD_SHA=HEAD bash "$gate_script" "$@" 2>&1)
  status=$?
  set -e
  if { [ "$expect" = pass ] && [ "$status" -ne 0 ]; } \
    || { [ "$expect" = fail ] && [ "$status" -ne 1 ]; }; then
    printf 'expected the gate to %s (%s), got exit %s:\n%s\n' "$expect" "$*" "$status" "$output" >&2
    exit 1
  fi
  printf '%s\n' "$output"
}

migration_fragment() {
  cat > "$1/changelog.d/9.breaking.md" <<'FRAGMENT'
- `harn run --old` is removed.

  Migration: pass `--new` instead.
FRAGMENT
}

# CLI surface: a removed line fails and names the entry; adding a line passes.
cli=$(new_repo cli)
printf '# header\nharn run\nharn run --old\n' > "$cli/spec/cli-surface.txt"
commit_all "$cli" base
cli_base=$(git -C "$cli" rev-parse HEAD)

printf '# header\nharn run\nharn run --new\nharn run --old\n' > "$cli/spec/cli-surface.txt"
commit_all "$cli" "add a flag"
run_gate "$cli" "$cli_base" pass cli | grep -q "no cli break"

printf '# header\nharn run\nharn run --new\n' > "$cli/spec/cli-surface.txt"
commit_all "$cli" "rename a flag"
run_gate "$cli" "$cli_base" fail cli | grep -qx "  - harn run --old"

# A breaking fragment without its migration declares nothing.
cat > "$cli/changelog.d/9.breaking.md" <<'FRAGMENT'
- `harn run --old` is removed.
FRAGMENT
commit_all "$cli" "fragment without migration"
run_gate "$cli" "$cli_base" fail cli | grep -q "9.breaking.md is a breaking change with no"

migration_fragment "$cli"
commit_all "$cli" "declare the break"
run_gate "$cli" "$cli_base" pass cli | grep -q "cli break declared by changelog.d/9.breaking.md"

# Deleting the listing removes every entry.
gone=$(new_repo gone)
printf 'harn run\n' > "$gone/spec/cli-surface.txt"
commit_all "$gone" base
gone_base=$(git -C "$gone" rev-parse HEAD)
git -C "$gone" rm -q spec/cli-surface.txt
commit_all "$gone" "delete the listing"
run_gate "$gone" "$gone_base" fail cli | grep -qx "  - harn run"

# Rust API: the verdict comes from the report's per-crate summaries.
api=$(new_repo api)
printf 'x\n' > "$api/README"
commit_all "$api" base
api_base=$(git -C "$api" rev-parse HEAD)
printf 'y\n' > "$api/README"
commit_all "$api" "change the API"

cat > "$tmp_root/clean.log" <<'REPORT'
    Checking harn-lexer v0.10.144 -> v0.10.144 (no change; assume minor)
     Checked [   0.013s] 196 checks: 196 pass, 58 skip
     Summary no semver update required
    Checking harn-glob v0.10.144 -> v0.10.144 (no change; assume minor)
     Checked [   0.010s] 196 checks: 195 pass, 1 fail, 0 warn, 58 skip
     Summary semver requires new minor version: 0 major and 1 minor checks failed
REPORT
cat > "$tmp_root/major.log" <<'REPORT'
    Checking harn-glob v0.10.144 -> v0.10.144 (no change; assume minor)
     Checked [   0.010s] 196 checks: 196 pass, 58 skip
     Summary no semver update required
    Checking harn-lexer v0.10.144 -> v0.10.144 (no change; assume minor)
     Checked [   0.006s] 196 checks: 195 pass, 1 fail, 0 warn, 58 skip

--- failure enum_variant_added: enum variant added on exhaustive enum ---

Failed in:
  variant StringSegment:ScratchFalsifier in crates/harn-lexer/src/token.rs:9

     Summary semver requires new major version: 1 major and 0 minor checks failed
REPORT
# The batched report compares prebuilt rustdoc, so cargo-semver-checks cannot
# name the crate; the gate's own `Crate` header does.
cat > "$tmp_root/batched.log" <<'REPORT'
       Crate harn-glob
    Checking <unknown> v0.10.144 -> v0.10.144 (no change; assume minor)
     Summary no semver update required
       Crate harn-vm
    Checking <unknown> v0.10.144 -> v0.10.144 (no change; assume minor)
     Summary semver requires new major version: 1 major and 0 minor checks failed
REPORT
# A pre-release version with no release type skips every lint: the summary
# says nothing is required because nothing ran.
cat > "$tmp_root/skipped.log" <<'REPORT'
       Crate harn-vm
    Checking <unknown> v0.10.145-dev -> v0.10.145-dev (no change; assume major)
     Checked [   0.000s] 0 checks: 0 pass, 254 skip
     Summary no semver update required
REPORT
# CI forces terminal colors; the verdict must survive them.
printf '       Crate harn-vm\n\033[1m\033[32m    Checking\033[0m <unknown> v1 -> v1\n\033[1m\033[32m     Checked\033[0m [ 0.1s] 196 checks: 195 pass, 1 fail\n\033[1m\033[33m     Summary\033[0m semver requires new major version: 1 major and 0 minor checks failed\n' \
  > "$tmp_root/colored.log"
head -4 "$tmp_root/major.log" > "$tmp_root/truncated.log"
: > "$tmp_root/empty.log"

run_gate "$api" "$api_base" pass rust-api "$tmp_root/clean.log" | grep -q "no rust-api break"
run_gate "$api" "$api_base" fail rust-api "$tmp_root/major.log" \
  | grep -qx "  - harn-lexer: semver requires new major version: 1 major and 0 minor checks failed"
run_gate "$api" "$api_base" fail rust-api "$tmp_root/batched.log" \
  | grep -qx "  - harn-vm: semver requires new major version: 1 major and 0 minor checks failed"
run_gate "$api" "$api_base" fail rust-api "$tmp_root/skipped.log" | grep -q "ran zero lints on 1 crate(s)"
run_gate "$api" "$api_base" fail rust-api "$tmp_root/colored.log" \
  | grep -qx "  - harn-vm: semver requires new major version: 1 major and 0 minor checks failed"
run_gate "$api" "$api_base" fail rust-api "$tmp_root/truncated.log" | grep -q "checked 2 crate(s) and summarized 1"
run_gate "$api" "$api_base" fail rust-api "$tmp_root/empty.log" | grep -q "missing or empty"

migration_fragment "$api"
commit_all "$api" "declare the break"
run_gate "$api" "$api_base" pass rust-api "$tmp_root/major.log" | grep -q "rust-api break declared"

echo "breaking_surface_check_test: ok"
