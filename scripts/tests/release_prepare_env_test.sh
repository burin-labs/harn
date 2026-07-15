#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
real_path=$PATH

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

release_root="$tmp_root/release-root"
mkdir -p \
  "$release_root/crates/example" \
  "$release_root/crates/excluded" \
  "$release_root/tree-sitter-harn" \
  "$release_root/docs/src/spec/language" \
  "$release_root/docs/theme"
cat > "$release_root/Cargo.toml" <<'EOF'
[workspace]
version = "1.2.3"
members = ["crates/example", "tree-sitter-harn"]
exclude = ["crates/excluded"]
resolver = "2"
EOF
cat > "$release_root/Cargo.lock" <<'EOF'
# fake lock
EOF
cat > "$release_root/crates/example/Cargo.toml" <<'EOF'
[package]
name = "example"
version = "1.2.3"
edition = "2021"

[dependencies]
tree-sitter-harn = { path = "../../tree-sitter-harn", version = "1.2", optional = true }
harn-excluded = { path = "../excluded", version = "1.2" }
serde = { version = "1", optional = true }
EOF
cat > "$release_root/tree-sitter-harn/Cargo.toml" <<'EOF'
[package]
name = "tree-sitter-harn"
version = "1.2.3"
edition = "2021"
EOF
cat > "$release_root/crates/excluded/Cargo.toml" <<'EOF'
[package]
name = "harn-excluded"
version = "1.2.3"
edition = "2021"
EOF
cat > "$release_root/CHANGELOG.md" <<'EOF'
# Changelog

## v1.2.4

- release notes
EOF
cat > "$release_root/docs/src/embedding-rust.md" <<'EOF'
tag = "v1.2.3"
EOF
touch \
  "$release_root/docs/src/language-spec.md" \
  "$release_root/docs/src/SUMMARY.md" \
  "$release_root/docs/theme/harn-keywords.js"

git -C "$release_root" init --quiet
git -C "$release_root" config user.name "Release Test"
git -C "$release_root" config user.email "release-test@example.com"
git -C "$release_root" add .
git -C "$release_root" commit --quiet -m "initial"
git -C "$release_root" checkout --quiet -b release/v1.2.4

fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin"

cat > "$fake_bin/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'argv=%s\n' "$*"
  printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL-__unset__}"
  printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-__unset__}"
  printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-__unset__}"
  printf 'SCCACHE_DISABLE=%s\n' "${SCCACHE_DISABLE-__unset__}"
} >> "$FAKE_HARN_RECORD"
exit 0
SH
chmod +x "$fake_bin/harn"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'argv=%s\n' "$*"
  printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL-__unset__}"
  printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-__unset__}"
  printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-__unset__}"
  printf 'SCCACHE_DISABLE=%s\n' "${SCCACHE_DISABLE-__unset__}"
} >> "$FAKE_CARGO_RECORD"
case "$*" in
  "metadata --format-version=1")
    printf '# reconciled by fake Cargo\n' >> Cargo.lock
    ;;
  "metadata --format-version=1 --locked")
    grep -Fq '# reconciled by fake Cargo' Cargo.lock
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_bin/cargo"

cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${FAIL_ON_MAKE:-0}" == "1" ]]; then
  echo "unexpected make invocation: $*" >&2
  exit 2
fi
if [[ "${ASSERT_DERIVED_PRE_BUMP:-0}" == "1" ]] \
  && [[ "$*" == "sync-language-spec" || "$*" == "gen-highlight" ]] \
  && grep -Fq 'version = "1.2.4"' Cargo.toml; then
  echo "derived target ran after the metadata version bump: $*" >&2
  exit 2
fi
if [[ -n "${SHIP_GATE_RECORD:-}" ]]; then
  printf 'make=%s\n' "$*" >> "$SHIP_GATE_RECORD"
fi
{
  printf 'target=%s\n' "$*"
  printf 'version=%s\n' "$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
  printf 'HARN_BIN=%s\n' "${HARN_BIN-__unset__}"
  printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL-__unset__}"
  printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-__unset__}"
  printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-__unset__}"
  printf 'SCCACHE_DISABLE=%s\n' "${SCCACHE_DISABLE-__unset__}"
} >> "$FAKE_MAKE_RECORD"
exit 0
SH
chmod +x "$fake_bin/make"

record_harn="$tmp_root/harn-record.txt"
record_cargo="$tmp_root/cargo-record.txt"
record_make="$tmp_root/make-record.txt"

HARN_RELEASE_ROOT="$release_root" \
HARN_BIN="$fake_bin/harn" \
FAKE_HARN_RECORD="$record_harn" \
FAKE_CARGO_RECORD="$record_cargo" \
FAKE_MAKE_RECORD="$record_make" \
FAIL_ON_MAKE=1 \
PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/release_gate.sh" prepare --bump minor

if ! grep -Fxq "argv=run scripts/sync_protocol_fixture_runtime_versions.harn -- --from 1.2.3 --to 1.3.0" "$record_harn"; then
  echo "release_gate prepare did not route fixture sync through HARN_BIN" >&2
  cat "$record_harn" >&2
  exit 1
fi

if ! grep -Fq 'tree-sitter-harn = { path = "../../tree-sitter-harn", version = "=1.3.0", optional = true }' "$release_root/crates/example/Cargo.toml"; then
  echo "release_gate prepare did not update root-level workspace member dependency versions to exact pins" >&2
  cat "$release_root/crates/example/Cargo.toml" >&2
  exit 1
fi
if ! grep -Fq 'harn-excluded = { path = "../excluded", version = "=1.3.0" }' "$release_root/crates/example/Cargo.toml"; then
  echo "release_gate prepare did not preserve excluded local crate dependency version rewrites" >&2
  cat "$release_root/crates/example/Cargo.toml" >&2
  exit 1
fi
if ! grep -Fq 'serde = { version = "1", optional = true }' "$release_root/crates/example/Cargo.toml"; then
  echo "release_gate prepare should not rewrite non-local dependency versions" >&2
  cat "$release_root/crates/example/Cargo.toml" >&2
  exit 1
fi

if ! grep -Fxq "argv=dump-protocol-artifacts --artifact-version 1.3.0" "$record_harn"; then
  echo "release_gate prepare did not generate explicitly versioned protocol artifacts through HARN_BIN" >&2
  cat "$record_harn" >&2
  exit 1
fi
if [[ $(grep -Fxc 'argv=metadata --format-version=1' "$record_cargo") -ne 1 ]] \
  || [[ $(grep -Fxc 'argv=metadata --format-version=1 --locked' "$record_cargo") -ne 1 ]]; then
  echo "release_gate prepare did not reconcile and verify Cargo.lock exactly once" >&2
  cat "$record_cargo" >&2
  exit 1
fi
for expected in CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= SCCACHE_DISABLE=1; do
  if ! grep -Fxq "$expected" "$record_cargo"; then
    echo "expected $expected in $record_cargo" >&2
    cat "$record_cargo" >&2
    exit 1
  fi
done

if ! grep -Fxq "CARGO_INCREMENTAL=0" "$record_harn"; then
  echo "expected CARGO_INCREMENTAL=0 in $record_harn" >&2
  cat "$record_harn" >&2
  exit 1
fi
if ! grep -Fxq "RUSTC_WRAPPER=" "$record_harn"; then
  echo "expected empty RUSTC_WRAPPER in $record_harn" >&2
  cat "$record_harn" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_RUSTC_WRAPPER=" "$record_harn"; then
  echo "expected empty CARGO_BUILD_RUSTC_WRAPPER in $record_harn" >&2
  cat "$record_harn" >&2
  exit 1
fi
if ! grep -Fxq "SCCACHE_DISABLE=1" "$record_harn"; then
  echo "expected SCCACHE_DISABLE=1 in $record_harn" >&2
  cat "$record_harn" >&2
  exit 1
fi

# Exercise the real Cargo lockfile boundary with a dependency-free workspace.
# This falsifier fails on the old prepare implementation: Cargo.toml advances,
# but the local package entry in Cargo.lock remains at the previous version.
real_release_root="$tmp_root/real-release-root"
mkdir -p "$real_release_root/crates/example/src" "$real_release_root/docs/src"
cat > "$real_release_root/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/example"]
resolver = "2"

[workspace.package]
version = "1.2.3"
EOF
cat > "$real_release_root/crates/example/Cargo.toml" <<'EOF'
[package]
name = "example"
version.workspace = true
edition = "2021"
EOF
cat > "$real_release_root/crates/example/src/lib.rs" <<'EOF'
pub fn example() {}
EOF
cat > "$real_release_root/docs/src/embedding-rust.md" <<'EOF'
tag = "v1.2.3"
EOF
PATH="$real_path" cargo generate-lockfile --manifest-path "$real_release_root/Cargo.toml" --offline
git -C "$real_release_root" init --quiet
git -C "$real_release_root" config user.name "Release Test"
git -C "$real_release_root" config user.email "release-test@example.com"
git -C "$real_release_root" add .
git -C "$real_release_root" commit --quiet -m "initial"

HARN_RELEASE_ROOT="$real_release_root" \
HARN_BIN="$fake_bin/harn" \
FAKE_HARN_RECORD="$record_harn" \
CARGO_TARGET_DIR="$tmp_root/real-target" \
PATH="$real_path" \
  "$repo_root/scripts/release_gate.sh" prepare --bump patch

if ! grep -A2 -F 'name = "example"' "$real_release_root/Cargo.lock" | grep -Fq 'version = "1.2.4"'; then
  echo "release_gate prepare left the real Cargo.lock package version stale" >&2
  cat "$real_release_root/Cargo.lock" >&2
  exit 1
fi
PATH="$real_path" cargo metadata \
  --manifest-path "$real_release_root/Cargo.toml" \
  --format-version=1 \
  --locked >/dev/null

git -C "$release_root" reset --hard --quiet HEAD
: >"$record_make"

ship_gate="$tmp_root/fake-release-gate.sh"
cat > "$ship_gate" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'gate=%s\n' "$*" >> "$SHIP_GATE_RECORD"
case "${1:-}" in
  audit)
    if [[ "${FAIL_RELEASE_AUDIT:-0}" == "1" && " $* " != *" --validate-only "* ]]; then
      echo "injected release audit failure" >&2
      exit 9
    fi
    mkdir -p "${CARGO_TARGET_DIR:?}/debug"
    cat > "$CARGO_TARGET_DIR/debug/harn" <<'BIN'
#!/usr/bin/env bash
exit 0
BIN
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    ;;
  publish)
    ;;
  prepare)
    printf 'prepare HARN_BIN=%s\n' "${HARN_BIN-__unset__}" >> "$SHIP_GATE_RECORD"
    if [[ "${INJECT_HIDDEN_INDEX_CHANGE:-0}" == "1" ]]; then
      git update-index --assume-unchanged Cargo.toml
    fi
    python3 - <<'PY'
from pathlib import Path
p = Path("Cargo.toml")
p.write_text(p.read_text().replace('version = "1.2.3"', 'version = "1.2.4"'))
lock = Path("Cargo.lock")
lock.write_text(lock.read_text() + "# touched\n")
PY
    ;;
  *)
    echo "unexpected release gate invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$ship_gate"

record_ship="$tmp_root/ship-gate-record.txt"
target_dir="$tmp_root/target"

run_ship_prepare() {
  local label="$1"
  shift
  git -C "$release_root" reset --hard --quiet HEAD
  : > "$record_make"
  : > "$record_ship"
  env -u HARN_BIN \
  HARN_RELEASE_ROOT="$release_root" \
  HARN_RELEASE_HARNESS=1 \
  HARN_RELEASE_GATE_SCRIPT="$ship_gate" \
  CARGO_TARGET_DIR="$target_dir" \
  SHIP_GATE_RECORD="$record_ship" \
  FAKE_MAKE_RECORD="$record_make" \
  ASSERT_DERIVED_PRE_BUMP=1 \
  PATH="$fake_bin:$PATH" \
    "$repo_root/scripts/release_ship.sh" \
      --prepare --bump patch --skip-dry-run "$@" > "$tmp_root/ship-$label.txt" 2>&1
}

assert_ordered_ship_events() {
  local label="$1"
  shift
  local previous=0
  local expected line
  for expected in "$@"; do
    line="$(grep -n -m 1 -Fx "$expected" "$record_ship" | cut -d: -f1 || true)"
    if [[ -z "$line" || "$line" -le "$previous" ]]; then
      echo "release_ship sequence mismatch for $label at: $expected" >&2
      cat "$record_ship" >&2
      exit 1
    fi
    previous="$line"
  done
}

rm -rf "$target_dir"
run_ship_prepare full

expected_harn="$target_dir/debug/harn"
if ! grep -Fxq "prepare HARN_BIN=$expected_harn" "$record_ship"; then
  echo "release_ship did not pass warmed HARN_BIN into release_gate prepare" >&2
  cat "$record_ship" >&2
  exit 1
fi

for target in sync-language-spec gen-highlight; do
  expected_record=$(printf 'target=%s\nversion=1.2.3' "$target")
  if ! grep -Fq "$expected_record" "$record_make"; then
    echo "release_ship did not run $target against the pre-bump source version" >&2
    cat "$record_make" >&2
    exit 1
  fi
done

assert_ordered_ship_events full \
  "gate=audit --validate-only" \
  "make=sync-language-spec" \
  "make=gen-highlight" \
  "gate=prepare --bump patch --allow-dirty" \
  "make=portal-check" \
  "gate=audit"

rm -rf "$target_dir"
receipt="$tmp_root/audit-receipt.json"
printf '{}\n' > "$receipt"
run_ship_prepare residual --audit-receipt "$receipt"
assert_ordered_ship_events residual \
  "gate=audit --validate-only --receipt $receipt" \
  "make=sync-language-spec" \
  "make=gen-highlight" \
  "gate=prepare --bump patch --allow-dirty" \
  "make=portal-check" \
  "gate=audit --receipt $receipt"

: > "$record_make"
: > "$record_ship"
if HARN_RELEASE_ROOT="$release_root" \
  HARN_RELEASE_HARNESS=1 \
  HARN_RELEASE_GATE_SCRIPT="$ship_gate" \
  CARGO_TARGET_DIR="$target_dir" \
  SHIP_GATE_RECORD="$record_ship" \
  FAKE_MAKE_RECORD="$record_make" \
  PATH="$fake_bin:$PATH" \
    "$repo_root/scripts/release_ship.sh" \
      --prepare --bump patch --skip-dry-run --skip-audit \
      > "$tmp_root/ship-removed-skip.txt" 2>&1; then
  echo "release_ship accepted removed --skip-audit" >&2
  exit 1
fi
if ! grep -Fq "error: unknown arg: --skip-audit" "$tmp_root/ship-removed-skip.txt"; then
  echo "release_ship did not reject removed --skip-audit" >&2
  cat "$tmp_root/ship-removed-skip.txt" >&2
  exit 1
fi
if [[ -s "$record_make" || -s "$record_ship" ]]; then
  echo "release_ship started work before rejecting removed --skip-audit" >&2
  cat "$record_make" "$record_ship" >&2
  exit 1
fi

git -C "$release_root" reset --hard --quiet HEAD
mkdir -p "$release_root/changelog.d"
printf '*.md text eol=lf\n' > "$release_root/.gitattributes"
printf 'rollback masker regression\n' > "$release_root/changelog.d/rollback-masker.fixed.md"
git -C "$release_root" add .gitattributes changelog.d/rollback-masker.fixed.md
git -C "$release_root" commit --quiet -m "add tracked changelog fragment"
rm "$release_root/changelog.d/rollback-masker.fixed.md"
printf '\n- authored before failed prepare\n' >> "$release_root/CHANGELOG.md"
git -C "$release_root" add CHANGELOG.md
printf '\nunstaged authored note\n' >> "$release_root/docs/src/embedding-rust.md"
printf 'untracked authored note\n' > "$release_root/AUTHORED.md"
baseline_diff="$tmp_root/prepare-baseline.diff"
git -C "$release_root" diff --binary HEAD -- > "$baseline_diff"
baseline_cached_diff="$tmp_root/prepare-baseline-cached.diff"
git -C "$release_root" diff --cached --binary HEAD -- > "$baseline_cached_diff"
baseline_unstaged_diff="$tmp_root/prepare-baseline-unstaged.diff"
git -C "$release_root" diff --binary -- > "$baseline_unstaged_diff"
baseline_status="$tmp_root/prepare-baseline.status"
git -C "$release_root" status --porcelain=v1 > "$baseline_status"
: > "$record_make"
: > "$record_ship"
set +e
HARN_RELEASE_ROOT="$release_root" \
  HARN_RELEASE_HARNESS=1 \
  HARN_RELEASE_GATE_SCRIPT="$ship_gate" \
  CARGO_TARGET_DIR="$target_dir" \
  SHIP_GATE_RECORD="$record_ship" \
  FAKE_MAKE_RECORD="$record_make" \
  FAIL_RELEASE_AUDIT=1 \
  INJECT_HIDDEN_INDEX_CHANGE=1 \
  PATH="$fake_bin:$PATH" \
    "$repo_root/scripts/release_ship.sh" \
      --prepare --bump patch --skip-dry-run \
      > "$tmp_root/ship-rollback.txt" 2>&1
rollback_rc=$?
set -e
if [[ "$rollback_rc" -eq 0 ]]; then
  echo "release_ship unexpectedly passed injected post-generation audit failure" >&2
  exit 1
fi
if [[ "$rollback_rc" -ne 9 ]]; then
  echo "release_ship did not preserve the original audit failure status" >&2
  echo "status: $rollback_rc" >&2
  cat "$tmp_root/ship-rollback.txt" >&2
  exit 1
fi
if ! grep -Fq "injected release audit failure" "$tmp_root/ship-rollback.txt"; then
  echo "release_ship output did not include the original audit failure" >&2
  cat "$tmp_root/ship-rollback.txt" >&2
  exit 1
fi
if grep -Fq "unable to stat" "$tmp_root/ship-rollback.txt"; then
  echo "release_ship rollback masked the audit failure with a deleted-fragment stat error" >&2
  cat "$tmp_root/ship-rollback.txt" >&2
  exit 1
fi
after_diff="$tmp_root/prepare-after.diff"
git -C "$release_root" diff --binary HEAD -- > "$after_diff"
if ! cmp -s "$baseline_diff" "$after_diff"; then
  echo "failed post-generation audit did not restore the authored release tree" >&2
  diff -u "$baseline_diff" "$after_diff" >&2 || true
  exit 1
fi
after_cached_diff="$tmp_root/prepare-after-cached.diff"
git -C "$release_root" diff --cached --binary HEAD -- > "$after_cached_diff"
if ! cmp -s "$baseline_cached_diff" "$after_cached_diff"; then
  echo "failed post-generation audit did not restore the authored index" >&2
  diff -u "$baseline_cached_diff" "$after_cached_diff" >&2 || true
  exit 1
fi
after_unstaged_diff="$tmp_root/prepare-after-unstaged.diff"
git -C "$release_root" diff --binary -- > "$after_unstaged_diff"
if ! cmp -s "$baseline_unstaged_diff" "$after_unstaged_diff"; then
  echo "failed post-generation audit did not restore the authored unstaged tree" >&2
  diff -u "$baseline_unstaged_diff" "$after_unstaged_diff" >&2 || true
  exit 1
fi
after_status="$tmp_root/prepare-after.status"
git -C "$release_root" status --porcelain=v1 > "$after_status"
if ! cmp -s "$baseline_status" "$after_status"; then
  echo "failed post-generation audit did not restore authored status" >&2
  diff -u "$baseline_status" "$after_status" >&2 || true
  exit 1
fi
if ! grep -Fq 'version = "1.2.3"' "$release_root/Cargo.toml"; then
  echo "failed post-generation audit left the version bump in place" >&2
  exit 1
fi

echo "release_prepare_env_test: ok"
