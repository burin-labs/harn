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
mkdir -p "$release_root/scripts"
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
  printf 'meta\tfalse\ttest\n'
  case "${FAKE_AUDIT_LANE:-security}" in
    rust)
      printf 'lane\trust-audit\trun_rust_audit\n'
      ;;
    parallel)
      printf 'lane\trust-audit\trun_rust_audit\n'
      printf 'lane\tsecurity-audit\trun_security_audit\n'
      ;;
    package)
      printf 'lane\tpackage-audit\trun_package_audit\n'
      ;;
    *)
      printf 'lane\tsecurity-audit\trun_security_audit\n'
      ;;
  esac
  exit 0
fi
echo "unexpected fake harn invocation: $*" >&2
exit 2
SH
chmod +x "$fake_harn"

cat > "$fake_bin/rg" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ -n "${FAKE_SYNC_FIFO:-}" ]]; then
  read -r token < "$FAKE_SYNC_FIFO"
  [[ "$token" == "rust-attempt-settled" ]]
  printf 'security-settled\n' >> "$FAKE_EVENT_RECORD"
fi
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
    if [[ -n "${FAKE_CARGO_ENV_RECORD:-}" ]]; then
      printf '%s\t%s\n' "$CARGO_TARGET_DIR" "$CARGO_BUILD_BUILD_DIR" >> "$FAKE_CARGO_ENV_RECORD"
    fi
    if [[ -n "${FAKE_EVENT_RECORD:-}" ]]; then
      printf 'cargo-clean\n' >> "$FAKE_EVENT_RECORD"
    fi
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

cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_MAKE_RECORD"
if [[ "${1:-}" == "fmt-check" && "${FAKE_MAKE_MODE:-}" == parallel-* ]]; then
  count=0
  if [[ -f "$FAKE_CARGO_STATE/fmt-count" ]]; then
    count=$(<"$FAKE_CARGO_STATE/fmt-count")
  fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$FAKE_CARGO_STATE/fmt-count"
  if [[ "$count" -eq 1 ]]; then
    printf 'rust-first-attempt-settled\n' >> "$FAKE_EVENT_RECORD"
    printf 'rust-attempt-settled\n' > "$FAKE_SYNC_FIFO"
    if [[ "$FAKE_MAKE_MODE" != "parallel-ordinary" ]]; then
      echo "error: couldn't read $CARGO_BUILD_BUILD_DIR/debug/build/tree-sitter-bb1d5a918bffdfb1/out/stdlib-symbols.txt: No such file or directory (os error 2)" >&2
    else
      echo 'error[E0308]: ordinary audit-lane compiler failure' >&2
    fi
    exit 2
  fi
  if [[ "$FAKE_MAKE_MODE" == "parallel-retry-fails" ]]; then
    echo 'error[E9999]: audit-lane retry compiler failure' >&2
    exit 2
  fi
fi
if [[ "${1:-}" == "gen-cli-aot" && "${FAKE_MAKE_MODE:-}" == "stale-aot" ]]; then
  count=0
  if [[ -f "$FAKE_CARGO_STATE/aot-count" ]]; then
    count=$(<"$FAKE_CARGO_STATE/aot-count")
  fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$FAKE_CARGO_STATE/aot-count"
  if [[ "$count" -eq 1 ]]; then
    echo "error: couldn't read $CARGO_BUILD_BUILD_DIR/debug/build/libsqlite3-sys-ec7fd4252cc18b37/out/bindgen.rs: No such file or directory (os error 2)" >&2
    exit 2
  fi
fi
SH
chmod +x "$fake_bin/make"

cat > "$fake_bin/env" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$fake_bin/env"

cat > "$release_root/scripts/verify_crate_packages.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
count=0
if [[ -f "$FAKE_CARGO_STATE/package-count" ]]; then
  count=$(<"$FAKE_CARGO_STATE/package-count")
fi
count=$((count + 1))
printf '%s\n' "$count" > "$FAKE_CARGO_STATE/package-count"
if [[ "$count" -eq 1 ]]; then
  package_build="$CARGO_TARGET_DIR/package-check-target/build"
  echo "error: couldn't read $package_build/debug/build/libsqlite3-sys-ec7fd4252cc18b37/out/bindgen.rs: No such file or directory (os error 2)" >&2
  exit 101
fi
SH
chmod +x "$release_root/scripts/verify_crate_packages.sh"

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
  : > "$state/make-record"
  set +e
  HARN_RELEASE_ROOT="$release_root" \
    HARN_BIN="$fake_harn" \
    CARGO_TARGET_DIR="$target" \
    CARGO_BUILD_BUILD_DIR="$build" \
    FAKE_CARGO_MODE="$mode" \
    FAKE_CARGO_RECORD="$state/cargo-record" \
    FAKE_CARGO_STATE="$state" \
    FAKE_MAKE_RECORD="$state/make-record" \
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
if ! grep -Fq 'recovery: stale Cargo build-script outputs detected for warm prebuild (packages=libsqlite3-sys,tree-sitter)' \
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

aot_state="$tmp_root/state-aot"
aot_target="$tmp_root/target aot"
aot_build="$tmp_root/build aot"
mkdir -p \
  "$aot_state" \
  "$aot_target/deps" \
  "$aot_build/debug/build/libsqlite3-sys-ec7fd4252cc18b37/out"
touch "$aot_target/deps/keep"
: > "$aot_state/cargo-record"
: > "$aot_state/make-record"
set +e
HARN_RELEASE_ROOT="$release_root" \
  HARN_BIN="$fake_harn" \
  CARGO_TARGET_DIR="$aot_target" \
  CARGO_BUILD_BUILD_DIR="$aot_build" \
  FAKE_AUDIT_LANE=rust \
  FAKE_CARGO_MODE=stale-then-success \
  FAKE_CARGO_RECORD="$aot_state/cargo-record" \
  FAKE_CARGO_STATE="$aot_state" \
  FAKE_MAKE_MODE=stale-aot \
  FAKE_MAKE_RECORD="$aot_state/make-record" \
  PATH="$fake_bin:$PATH" \
  "$release_tools/release_gate.sh" audit --source-only > "$aot_state/output" 2>&1
aot_status=$?
set -e
if [[ "$aot_status" -ne 0 ]]; then
  cat "$aot_state/output" >&2
  exit 1
fi
if [[ "$(grep -c '^gen-cli-aot$' "$aot_state/make-record")" -ne 2 ]]; then
  echo "source-only AOT recovery should generate exactly twice" >&2
  cat "$aot_state/make-record" >&2
  exit 1
fi
if ! grep -Fxq 'clean -p libsqlite3-sys' "$aot_state/cargo-record"; then
  echo "source-only AOT recovery should clean only the implicated package" >&2
  cat "$aot_state/cargo-record" >&2
  exit 1
fi
if grep -q '^build ' "$aot_state/cargo-record"; then
  echo "source-only AOT recovery unexpectedly rebuilt the warm Harn binary" >&2
  cat "$aot_state/cargo-record" >&2
  exit 1
fi
if [[ ! -f "$aot_target/deps/keep" ]]; then
  echo "source-only AOT recovery discarded unrelated target artifacts" >&2
  exit 1
fi
grep -Fq 'recovery: shared CLI AOT preparation succeeded after package-scoped cleanup' \
  "$aot_state/output"

run_parallel_case() {
  local label="$1"
  local make_mode="$2"
  local state="$tmp_root/state-parallel-$label"
  local target="$tmp_root/target parallel $label"
  local build="$tmp_root/build parallel $label"
  local fifo="$state/lane-sync"
  mkdir -p \
    "$state" \
    "$target/deps" \
    "$build/debug/build/tree-sitter-bb1d5a918bffdfb1/out"
  mkfifo "$fifo"
  : > "$state/cargo-record"
  : > "$state/make-record"
  : > "$state/event-record"
  set +e
  HARN_RELEASE_ROOT="$release_root" \
    HARN_BIN="$fake_harn" \
    CARGO_TARGET_DIR="$target" \
    CARGO_BUILD_BUILD_DIR="$build" \
    FAKE_AUDIT_LANE=parallel \
    FAKE_CARGO_MODE=stale-then-success \
    FAKE_CARGO_RECORD="$state/cargo-record" \
    FAKE_CARGO_STATE="$state" \
    FAKE_EVENT_RECORD="$state/event-record" \
    FAKE_MAKE_MODE="$make_mode" \
    FAKE_MAKE_RECORD="$state/make-record" \
    FAKE_SYNC_FIFO="$fifo" \
    PATH="$fake_bin:$PATH" \
    "$release_tools/release_gate.sh" audit --source-only > "$state/output" 2>&1
  local status=$?
  set -e
  printf '%s\n' "$status" > "$state/status"
  printf '%s\n' "$state"
}

parallel_state=$(run_parallel_case recovery parallel-stale)
if [[ "$(<"$parallel_state/status")" -ne 0 ]]; then
  cat "$parallel_state/output" >&2
  exit 1
fi
if [[ "$(grep -c '^fmt-check$' "$parallel_state/make-record")" -ne 2 ]]; then
  echo "recoverable parallel lane should retry exactly once" >&2
  cat "$parallel_state/make-record" >&2
  exit 1
fi
if [[ "$(grep -c '^clean -p tree-sitter$' "$parallel_state/cargo-record")" -ne 1 ]]; then
  echo "parallel recovery should clean only the implicated package once" >&2
  cat "$parallel_state/cargo-record" >&2
  exit 1
fi
if [[ "$(paste -sd, "$parallel_state/event-record")" != \
  "rust-first-attempt-settled,security-settled,cargo-clean" ]]; then
  echo "parallel recovery cleanup ran before every sibling lane settled" >&2
  cat "$parallel_state/event-record" >&2
  exit 1
fi
if ! grep -Fq 'recovery: retrying rust-audit once after every initial audit lane settled' \
  "$parallel_state/output"; then
  echo "parallel recovery telemetry is missing" >&2
  cat "$parallel_state/output" >&2
  exit 1
fi

ordinary_parallel_state=$(run_parallel_case ordinary parallel-ordinary)
if [[ "$(<"$ordinary_parallel_state/status")" -eq 0 ]]; then
  echo "ordinary parallel lane failure should remain failed" >&2
  exit 1
fi
if [[ "$(grep -c '^fmt-check$' "$ordinary_parallel_state/make-record")" -ne 1 ]] \
  || grep -q '^clean ' "$ordinary_parallel_state/cargo-record"; then
  echo "ordinary parallel lane failure should not retry or clean" >&2
  cat "$ordinary_parallel_state/make-record" >&2
  cat "$ordinary_parallel_state/cargo-record" >&2
  exit 1
fi
grep -Fq 'error[E0308]: ordinary audit-lane compiler failure' \
  "$ordinary_parallel_state/output"

failed_retry_state=$(run_parallel_case failed-retry parallel-retry-fails)
if [[ "$(<"$failed_retry_state/status")" -eq 0 ]]; then
  echo "failed parallel recovery retry should remain failed" >&2
  exit 1
fi
if [[ "$(grep -c '^fmt-check$' "$failed_retry_state/make-record")" -ne 2 ]] \
  || [[ "$(grep -c '^clean -p tree-sitter$' "$failed_retry_state/cargo-record")" -ne 1 ]]; then
  echo "failed parallel recovery should clean once and retry once" >&2
  cat "$failed_retry_state/make-record" >&2
  cat "$failed_retry_state/cargo-record" >&2
  exit 1
fi
grep -Fq 'No such file or directory' "$failed_retry_state/output"
grep -Fq 'error[E9999]: audit-lane retry compiler failure' \
  "$failed_retry_state/output"
grep -Fq -- '--- rust-audit (first attempt, before stale-output recovery) ---' \
  "$failed_retry_state/output"
grep -Fq -- '--- rust-audit (terminal attempt) ---' \
  "$failed_retry_state/output"

package_state="$tmp_root/state-package"
package_target="$tmp_root/target package"
package_build="$tmp_root/build package"
mkdir -p "$package_state" "$package_target" "$package_build"
: > "$package_state/cargo-record"
: > "$package_state/cargo-env-record"
: > "$package_state/make-record"
set +e
HARN_RELEASE_ROOT="$release_root" \
  HARN_BIN="$fake_harn" \
  CARGO_TARGET_DIR="$package_target" \
  CARGO_BUILD_BUILD_DIR="$package_build" \
  FAKE_AUDIT_LANE=package \
  FAKE_CARGO_MODE=stale-then-success \
  FAKE_CARGO_RECORD="$package_state/cargo-record" \
  FAKE_CARGO_ENV_RECORD="$package_state/cargo-env-record" \
  FAKE_CARGO_STATE="$package_state" \
  FAKE_MAKE_RECORD="$package_state/make-record" \
  PATH="$fake_bin:$PATH" \
  "$release_tools/release_gate.sh" audit --source-only > "$package_state/output" 2>&1
package_status=$?
set -e
if [[ "$package_status" -ne 0 ]]; then
  cat "$package_state/output" >&2
  exit 1
fi
if [[ "$(<"$package_state/package-count")" -ne 2 ]]; then
  echo "recoverable package audit should retry exactly once" >&2
  cat "$package_state/output" >&2
  exit 1
fi
if [[ "$(<"$package_state/cargo-env-record")" != \
  "$package_target/package-check-target"$'\t'"$package_target/package-check-target/build" ]]; then
  echo "package-audit recovery should clean in the package verification Cargo context" >&2
  cat "$package_state/cargo-env-record" >&2
  exit 1
fi
if ! grep -Fxq 'clean -p libsqlite3-sys' "$package_state/cargo-record"; then
  echo "package-audit recovery should clean only the implicated package" >&2
  cat "$package_state/cargo-record" >&2
  exit 1
fi

echo "release_gate_stale_out_dir_test: ok"
