#!/usr/bin/env bash
# Hook wiring for the warn-only generated-artifact drift advisory.
# Matching logic lives in scripts/warn_generated_artifact_drift.harn
# (covered by scripts/tests/warn_generated_artifact_drift_test.harn).
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fail() {
  echo "hook_generated_artifact_drift_warn_test: $*" >&2
  exit 1
}

# The hook executes the production warner from a temporary repository. Keep
# its source bundle explicit here so a new relative import cannot silently turn
# the advisory into a non-blocking runtime error.
copy_drift_warner_fixture() {
  local fixture_root=$1
  mkdir -p "$fixture_root/scripts"
  cp "$repo_root/scripts/warn_generated_artifact_drift.harn" "$fixture_root/scripts/"
  cp "$repo_root/scripts/generated_artifacts_schema.harn" "$fixture_root/scripts/"
}

# shellcheck source=/dev/null
source "$repo_root/.githooks/lib.sh"

# Preserve the ambient CI/worktree binary before stubbing HARN_BIN for wiring checks.
saved_harn_bin=${HARN_BIN:-}
real_harn=$saved_harn_bin
if [ -z "$real_harn" ] || [ ! -x "$real_harn" ]; then
  real_harn=$(command -v harn || true)
fi
if [ -z "$real_harn" ] || [ ! -x "$real_harn" ]; then
  real_harn="$repo_root/target/debug/harn"
fi
[ -x "$real_harn" ] || fail "need an executable harn (HARN_BIN or PATH) to exercise pre-commit advisory"

record="$tmp_root/record.txt"
fake_harn="$tmp_root/fake-harn"
cat > "$fake_harn" <<'SH'
#!/bin/sh
printf 'run:%s\n' "$*" >> "${HOOK_DRIFT_RECORD:?}"
exit "${HOOK_DRIFT_FAKE_STATUS:-0}"
SH
chmod +x "$fake_harn"

export HOOK_DRIFT_RECORD="$record"

# Run from a fake repo root so the hook can stage the path list under .harn/tmp.
wire_root="$tmp_root/wire-root"
copy_drift_warner_fixture "$wire_root"
git -C "$wire_root" init --quiet
staged="$tmp_root/staged.txt"
printf '%s\n' 'crates/harn-lexer/src/token.rs' > "$staged"
: > "$record"
(
  cd "$wire_root"
  HARN_BIN="$fake_harn" hook_warn_generated_artifact_drift "$staged"
)
if ! grep -Eq '^run:run scripts/warn_generated_artifact_drift\.harn -- --staged-files .*/\.harn/tmp/staged-paths\.' "$record"; then
  fail "hook should invoke Harn warner with in-repo staged list; got: $(cat "$record")"
fi

# Without an explicit override, prefer the repository's no-build resolver to a
# PATH binary that may belong to another checkout or be replaced mid-build.
resolver_bin="$tmp_root/resolver-bin"
mkdir -p "$resolver_bin"
cat > "$resolver_bin/harn" <<'SH'
#!/bin/sh
exit 91
SH
chmod +x "$resolver_bin/harn"
cat > "$wire_root/scripts/harn_bin.sh" <<'SH'
#!/bin/sh
printf '%s\n' "${HOOK_RESOLVED_HARN:?}"
SH
chmod +x "$wire_root/scripts/harn_bin.sh"
: > "$record"
(
  cd "$wire_root"
  unset HARN_BIN
  PATH="$resolver_bin:$PATH" HOOK_RESOLVED_HARN="$fake_harn" \
    hook_warn_generated_artifact_drift "$staged"
)
grep -Fq 'run:run scripts/warn_generated_artifact_drift.harn' "$record" \
  || fail "hook should prefer the worktree Harn resolver"

# An advisory failure must remain non-blocking but visible. In particular, an
# out-of-memory SIGKILL exits 137 and must not look like a successful check.
set +e
failed_advisory_out=$(
  cd "$wire_root"
  HOOK_DRIFT_FAKE_STATUS=137 HARN_BIN="$fake_harn" \
    hook_warn_generated_artifact_drift "$staged" 2>&1
)
status=$?
set -e
[ "$status" -eq 0 ] || fail "failed advisory must still exit 0"
printf '%s\n' "$failed_advisory_out" \
  | grep -Fq "generated-artifact drift check did not run (harn exited 137)" \
  || fail "failed advisory must explain that it did not run; out=$failed_advisory_out"

# Missing binary must not fail the commit path.
set +e
(
  unset HARN_BIN HARN_BIN_NO_BUILD
  PATH="/nonexistent"
  cd "$tmp_root"
  # No scripts/warn… here and no harn on PATH.
  hook_warn_generated_artifact_drift "$staged"
)
status=$?
set -e
[ "$status" -eq 0 ] || fail "missing harn must still exit 0"

# Pre-commit surfaces the advisory and still succeeds (fast default).
work="$tmp_root/work"
fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin" "$work/.githooks" "$work/crates/harn-lexer/src" "$work/scripts"
cp "$repo_root/.githooks/lib.sh" "$work/.githooks/lib.sh"
cp "$repo_root/.githooks/pre-commit" "$work/.githooks/pre-commit"
copy_drift_warner_fixture "$work"
cp "$repo_root/scripts/generated_artifacts.toml" "$work/scripts/generated_artifacts.toml"
chmod +x "$work/.githooks/pre-commit"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
exit 0
SH
chmod +x "$fake_bin/cargo"

cat > "$work/Cargo.toml" <<'TOML'
[workspace]
members = ["crates/harn-lexer"]
resolver = "2"
TOML
cat > "$work/crates/harn-lexer/Cargo.toml" <<'TOML'
[package]
name = "harn-lexer"
version = "0.0.0"
edition = "2021"
TOML
printf 'pub fn token() {}\n' > "$work/crates/harn-lexer/src/lib.rs"
printf 'pub const KEYWORDS: &[&str] = &["fn"];\n' > "$work/crates/harn-lexer/src/token.rs"

git -C "$work" init --quiet
git -C "$work" config user.email "test@example.com"
git -C "$work" config user.name "Test User"
git -C "$work" config commit.gpgsign false
git -C "$work" add .
git -C "$work" commit --quiet -m base

printf 'pub const KEYWORDS: &[&str] = &["fn", "let"];\n' > "$work/crates/harn-lexer/src/token.rs"
git -C "$work" add crates/harn-lexer/src/token.rs

hook_out="$tmp_root/hook.out"
set +e
(
  cd "$work"
  PATH="$fake_bin:$PATH" \
    HARN_BIN="$real_harn" \
    HOOK_TIMING_LOG_DIR="$tmp_root/timings" \
    ./.githooks/pre-commit >"$hook_out" 2>&1
)
status=$?
set -e
[ "$status" -eq 0 ] || fail "pre-commit should succeed with advisory; got $status; out=$(cat "$hook_out")"
grep -Fq "generated artifact 'highlight'" "$hook_out" \
  || fail "pre-commit should surface highlight advisory; out=$(cat "$hook_out")"
grep -Fq "make gen-highlight" "$hook_out" \
  || fail "pre-commit advisory should name make gen-highlight; out=$(cat "$hook_out")"

echo "hook_generated_artifact_drift_warn_test: ok"
