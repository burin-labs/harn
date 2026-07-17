#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

release_root="$tmp_root/release root"
release_tools="$tmp_root/release tools"
fake_bin="$tmp_root/fake bin"
mkdir -p "$release_root" "$release_tools/lib" "$fake_bin"
cp "$repo_root/scripts/release_gate.sh" "$release_tools/release_gate.sh"
cp "$repo_root/scripts/harn_bin.sh" "$release_tools/harn_bin.sh"
cp -R "$repo_root/scripts/lib/." "$release_tools/lib/"

cat > "$release_root/Cargo.toml" <<'EOF'
[workspace]
version = "1.2.3"
members = []
EOF
mkdir -p "$release_root/docs/src" "$release_root/crates/harn-vm" "$release_root/crates/harn-cli" "$release_root/.github"
touch "$release_root/README.md" "$release_root/CLAUDE.md"
git -C "$release_root" init -q
git -C "$release_root" config user.email test@example.com
git -C "$release_root" config user.name test
git -C "$release_root" add .
git -C "$release_root" commit -qm init

fake_harn="$fake_bin/harn"
cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "run" && "${2:-}" == "scripts/release_audit_contract.harn" ]]; then
  printf '%s\n' '{"ok":true,"receipt_reused":false,"reason":"test","proof_kind":"full_local","head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","lane_names":["security-audit"],"lane_runners":["run_security_audit"],"lanes":[],"errors":[]}'
  exit 0
fi
echo "unexpected fake harn invocation: $*" >&2
exit 2
SH
chmod +x "$fake_harn"

cat > "$fake_bin/rg" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$fake_bin/rg"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_CARGO_RECORD"
case "${1:-}" in
  build)
    count=0
    if [[ -f "$FAKE_CARGO_STATE/build-count" ]]; then
      count=$(<"$FAKE_CARGO_STATE/build-count")
    fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$FAKE_CARGO_STATE/build-count"
    if [[ "$FAKE_CARGO_MODE" == "compiler" ]]; then
      echo 'error[E0308]: ordinary compiler failure' >&2
      exit 101
    fi
    if [[ "$count" -eq 1 ]]; then
      if [[ "$FAKE_CARGO_MODE" == "outside" ]]; then
        echo "error: couldn't read $CARGO_TARGET_DIR/unrelated/debug/build/tree-sitter-bb1d5a918bffdfb1/out/file: No such file or directory (os error 2)" >&2
        exit 101
      fi
      if [[ "$FAKE_CARGO_MODE" == "malformed" ]]; then
        echo "error: couldn't read $CARGO_BUILD_BUILD_DIR/debug/build/tree-sitter-not-a-hash/out/file: No such file or directory (os error 2)" >&2
        exit 101
      fi
      echo "error: couldn't read $CARGO_BUILD_BUILD_DIR/debug/build/tree-sitter-bb1d5a918bffdfb1/out/stdlib-symbols.txt: No such file or directory (os error 2)" >&2
      echo "error: couldn't read $CARGO_BUILD_BUILD_DIR/debug/build/libsqlite3-sys-ec7fd4252cc18b37/out/bindgen.rs: No such file or directory (os error 2)" >&2
      exit 101
    fi
    if [[ "$FAKE_CARGO_MODE" == "retry-fails" ]]; then
      echo 'error[E9999]: retry compiler failure' >&2
      exit 101
    fi
    mkdir -p "$CARGO_TARGET_DIR/debug"
    cp "$HARN_BIN" "$CARGO_TARGET_DIR/debug/harn"
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    ;;
  clean)
    if [[ "$FAKE_CARGO_MODE" == "clean-fails" ]]; then
      echo 'cargo clean failed' >&2
      exit 1
    fi
    rm -rf \
      "$CARGO_BUILD_BUILD_DIR/debug/build/libsqlite3-sys-ec7fd4252cc18b37" \
      "$CARGO_BUILD_BUILD_DIR/debug/build/tree-sitter-bb1d5a918bffdfb1"
    ;;
  *)
    echo "unexpected fake cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_bin/cargo"

run_case() {
  local label="$1"
  local mode="$2"
  local state="$tmp_root/state-$label"
  local target="$tmp_root/target $label"
  local build="$tmp_root/build $label"
  mkdir -p \
    "$state" \
    "$target/deps" \
    "$target/incremental" \
    "$build/debug/build/libsqlite3-sys-ec7fd4252cc18b37/out" \
    "$build/debug/build/tree-sitter-bb1d5a918bffdfb1/out"
  touch "$target/deps/keep" "$target/incremental/keep"
  : > "$state/cargo-record"
  set +e
  HARN_RELEASE_ROOT="$release_root" \
    HARN_BIN="$fake_harn" \
    CARGO_TARGET_DIR="$target" \
    CARGO_BUILD_BUILD_DIR="$build" \
    FAKE_CARGO_MODE="$mode" \
    FAKE_CARGO_RECORD="$state/cargo-record" \
    FAKE_CARGO_STATE="$state" \
    PATH="$fake_bin:$PATH" \
    "$release_tools/release_gate.sh" audit > "$state/output" 2>&1
  local status=$?
  set -e
  printf '%s\n' "$status" > "$state/status"
  printf '%s\n' "$state"
}

assert_failed_without_recovery() {
  local state="$1"
  local description="$2"
  if [[ "$(<"$state/status")" -eq 0 ]]; then
    echo "$description should remain failed" >&2
    exit 1
  fi
  if [[ "$(grep -c '^build -p harn-cli --bin harn --quiet$' "$state/cargo-record")" -ne 1 ]] \
    || grep -q '^clean ' "$state/cargo-record"; then
    echo "$description should not retry or clean" >&2
    cat "$state/cargo-record" >&2
    exit 1
  fi
}

success_state=$(run_case success stale-then-success)
if [[ "$(<"$success_state/status")" -ne 0 ]]; then
  cat "$success_state/output" >&2
  exit 1
fi
if [[ "$(grep -c '^build -p harn-cli --bin harn --quiet$' "$success_state/cargo-record")" -ne 2 ]]; then
  echo "stale-output recovery should build exactly twice" >&2
  cat "$success_state/cargo-record" >&2
  exit 1
fi
if ! grep -Fxq 'clean -p libsqlite3-sys -p tree-sitter' "$success_state/cargo-record"; then
  echo "stale-output recovery should clean only sorted implicated packages" >&2
  cat "$success_state/cargo-record" >&2
  exit 1
fi
if ! grep -Fq 'recovery: stale Cargo build-script outputs detected (packages=libsqlite3-sys,tree-sitter)' \
  "$success_state/output"; then
  echo "stale-output recovery telemetry is missing" >&2
  cat "$success_state/output" >&2
  exit 1
fi
if [[ ! -f "$tmp_root/target success/deps/keep" || ! -f "$tmp_root/target success/incremental/keep" ]]; then
  echo "package-scoped recovery discarded unrelated target artifacts" >&2
  exit 1
fi

compiler_state=$(run_case compiler compiler)
assert_failed_without_recovery "$compiler_state" "ordinary compiler failure"
grep -Fq 'error[E0308]: ordinary compiler failure' "$compiler_state/output"

outside_state=$(run_case outside outside)
assert_failed_without_recovery "$outside_state" "missing output outside the active Cargo build directory"

malformed_state=$(run_case malformed malformed)
assert_failed_without_recovery "$malformed_state" "malformed active-build output path"
grep -Fq 'stale-output classification failed closed' "$malformed_state/output"

retry_state=$(run_case retry retry-fails)
if [[ "$(<"$retry_state/status")" -eq 0 ]]; then
  echo "failed recovery retry should remain failed" >&2
  exit 1
fi
if [[ "$(grep -c '^build -p harn-cli --bin harn --quiet$' "$retry_state/cargo-record")" -ne 2 ]] \
  || [[ "$(grep -c '^clean -p libsqlite3-sys -p tree-sitter$' "$retry_state/cargo-record")" -ne 1 ]]; then
  echo "failed recovery should clean once and retry once" >&2
  cat "$retry_state/cargo-record" >&2
  exit 1
fi
grep -Fq "No such file or directory" "$retry_state/output"
grep -Fq 'error[E9999]: retry compiler failure' "$retry_state/output"

clean_state=$(run_case clean clean-fails)
if [[ "$(<"$clean_state/status")" -eq 0 ]]; then
  echo "failed package cleanup should remain failed" >&2
  exit 1
fi
if [[ "$(grep -c '^build -p harn-cli --bin harn --quiet$' "$clean_state/cargo-record")" -ne 1 ]] \
  || [[ "$(grep -c '^clean -p libsqlite3-sys -p tree-sitter$' "$clean_state/cargo-record")" -ne 1 ]]; then
  echo "cleanup failure should not retry the build" >&2
  cat "$clean_state/cargo-record" >&2
  exit 1
fi

echo "release_gate_stale_out_dir_test: ok"
