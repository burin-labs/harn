#!/usr/bin/env bash
set -euo pipefail

# This suite owns fake Cargo behavior, not shared rust-heavy scheduling.
export HARN_CARGO_LEASE_MODE=off

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

release_root="$tmp_root/release root"
release_tools="$tmp_root/release tools"
fake_bin="$tmp_root/fake bin"
mkdir -p "$release_root" "$release_tools/lib" "$fake_bin"
cp "$repo_root/scripts/release_gate.sh" "$release_tools/release_gate.sh"
cp "$repo_root/scripts/harn_bin.sh" "$release_tools/harn_bin.sh"
cp "$repo_root/scripts/cargo_with_worktree_build_dir.sh" \
  "$release_tools/cargo_with_worktree_build_dir.sh"
cp -R "$repo_root/scripts/lib/." "$release_tools/lib/"
chmod +x "$release_tools/cargo_with_worktree_build_dir.sh"

cat > "$release_root/Cargo.toml" <<'EOF'
[workspace]
version = "1.2.3"
members = []
EOF
mkdir -p "$release_root/docs/src" "$release_root/crates/harn-vm" "$release_root/crates/harn-cli" "$release_root/.github"
mkdir -p "$release_root/scripts/ci"
touch "$release_root/README.md" "$release_root/CLAUDE.md"
git -C "$release_root" init -q
git -C "$release_root" config user.email test@example.com
git -C "$release_root" config user.name test
git -C "$release_root" config commit.gpgsign false
git -C "$release_root" add .
git -C "$release_root" commit -qm init

fake_harn="$fake_bin/harn"
cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "__internal-freshness-evidence-v5" ]]; then
  if [[ -n "${7:-}" ]]; then printf "harn-freshness-manifest-v4\n" >"$7"; fi
  binary_hash="$(git hash-object --no-filters -- "$3")000000000000000000000000"
  dep_hash="$(git hash-object --no-filters -- "$2")000000000000000000000000"
  printf 'harn-artifact-evidence-v5-cargo-output-dep-info-v1-manifest-3\nbuild-freshness=%s\nbuild-id=%s\nartifact-stat=%s\ndep-info=%s\ndependencies=%s\n' \
    "$(cat "$3.build-freshness")" "$binary_hash" "$binary_hash" \
    "$dep_hash" "$dep_hash"
  exit 0
fi
if [[ "${1:-}" == "__internal-executable-path" ]]; then
  printf '%s\n' "$0"
  exit 0
fi
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
    constrained)
      printf 'lane\trust-audit\trun_rust_audit\n'
      printf 'lane\tharn-audit\trun_harn_audit\n'
      printf 'lane\tsecurity-audit\trun_security_audit\n'
      printf 'lane\tpackage-audit\trun_package_audit\n'
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
if [[ -n "${FAKE_LANE_ENV_RECORD:-}" ]]; then
  printf 'cargo\t%s\t%s\t%s\n' "$*" "${CARGO_BUILD_JOBS-}" "${HARN_CONFORMANCE_JOBS-}" \
    >> "$FAKE_LANE_ENV_RECORD"
fi
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
    # Cargo stops at the FIRST missing build-script output, so a decayed cache
    # reveals one package per attempt. Recovery has to keep going.
    if [[ "$FAKE_CARGO_MODE" == "cascading" ]]; then
      if [[ "$count" -eq 1 ]]; then
        echo "error: couldn't read $CARGO_BUILD_BUILD_DIR/debug/build/libsqlite3-sys-ec7fd4252cc18b37/out/bindgen.rs: No such file or directory (os error 2)" >&2
        exit 101
      fi
      if [[ "$count" -eq 2 ]]; then
        echo "error: couldn't read $CARGO_BUILD_BUILD_DIR/debug/build/tree-sitter-bb1d5a918bffdfb1/out/stdlib-symbols.txt: No such file or directory (os error 2)" >&2
        exit 101
      fi
    fi
    # Decay that per-package cleanup cannot reach: the same package keeps
    # failing until the whole target directory is discarded.
    if [[ "$FAKE_CARGO_MODE" == "unreachable" ]]; then
      if [[ -f "$CARGO_TARGET_DIR/sentinel" ]]; then
        echo "error: couldn't read $CARGO_BUILD_BUILD_DIR/debug/build/libsqlite3-sys-ec7fd4252cc18b37/out/bindgen.rs: No such file or directory (os error 2)" >&2
        exit 101
      fi
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
    cp "${0%/*}/harn" "$CARGO_TARGET_DIR/debug/harn"
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    if [[ "$*" == *"--bin harn-freshness-check"* ]]; then
      cp "${0%/*}/harn-freshness-check" "$CARGO_TARGET_DIR/debug/harn-freshness-check"
      chmod +x "$CARGO_TARGET_DIR/debug/harn-freshness-check"
    fi
    cp "$CARGO_TARGET_DIR/debug/harn" "$CARGO_TARGET_DIR/debug/harn.fixture-template"
    escaped_harn="${CARGO_TARGET_DIR// /\\ }/debug/harn"
    printf '%s:\n' "$escaped_harn" > "$CARGO_TARGET_DIR/debug/harn.d"
    printf '%s\n' "${HARN_BUILD_FRESHNESS_ID:-0000000000000000000000000000000000000000}" \
      > "$CARGO_TARGET_DIR/debug/harn.build-freshness"
    ;;
  run)
    if [[ "$*" != "run --quiet --bin harn -- __internal-executable-path" ]]; then
      echo "unexpected fake cargo invocation: $*" >&2
      exit 2
    fi
    if [[ ! -x "$CARGO_TARGET_DIR/debug/harn" ]]; then
      cp "$CARGO_TARGET_DIR/debug/harn.fixture-template" "$CARGO_TARGET_DIR/debug/harn"
      chmod +x "$CARGO_TARGET_DIR/debug/harn"
    fi
    printf '%s\n' "${HARN_BUILD_FRESHNESS_ID:?}" \
      > "$CARGO_TARGET_DIR/debug/harn.build-freshness"
    printf '%s/debug/harn\n' "$CARGO_TARGET_DIR"
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
cp "$repo_root/scripts/tests/fixtures/harn_bin/fake_freshness_checker.sh" \
  "$fake_bin/harn-freshness-check"
chmod +x "$fake_bin/harn-freshness-check"

cat > "$fake_bin/cargo-nextest" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$fake_bin/cargo-nextest"

cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_MAKE_RECORD"
if [[ -n "${FAKE_LANE_ENV_RECORD:-}" ]]; then
  printf 'make\t%s\t%s\t%s\n' "$*" "${CARGO_BUILD_JOBS-}" "${HARN_CONFORMANCE_JOBS-}" \
    >> "$FAKE_LANE_ENV_RECORD"
fi
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

cat > "$release_root/scripts/ci/run_rust_lint_lane.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'rust-lint-lane-entrypoint\n' >> "$FAKE_MAKE_RECORD"
SH
chmod +x "$release_root/scripts/ci/run_rust_lint_lane.sh"

cat > "$release_root/scripts/verify_crate_packages.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${FAKE_PACKAGE_MODE:-}" == "sigkill" ]]; then
  kill -9 $$
fi
count=0
if [[ -f "$FAKE_CARGO_STATE/package-count" ]]; then
  count=$(<"$FAKE_CARGO_STATE/package-count")
fi
count=$((count + 1))
printf '%s\n' "$count" > "$FAKE_CARGO_STATE/package-count"
if [[ "$count" -eq 1 ]]; then
  package_build="$HARN_PACKAGE_VERIFY_BUILD_DIR"
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
  # Survives package-scoped `cargo clean`; disappears only if the whole target
  # directory is discarded.
  touch "$target/deps/keep" "$target/incremental/keep" "$target/sentinel"
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
  cat "$success_state/cargo-record" >&2
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
if ! grep -Fxq 'lint-no-rust-prompt-prose' "$aot_state/make-record" \
  || ! grep -Fxq 'rust-lint-lane-entrypoint' "$aot_state/make-record"; then
  echo "release Rust audit should keep the prompt policy and Clippy entrypoint as separate proofs" >&2
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
grep -Fq 'recovery: shared CLI AOT preparation succeeded after stale build-script cleanup' \
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
    HARN_RELEASE_GATE_LANE_CPUS=64 \
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
if ! grep -Fq 'recovery: retrying rust-audit (round 1 of' \
  "$parallel_state/output"; then
  echo "parallel recovery telemetry is missing" >&2
  cat "$parallel_state/output" >&2
  exit 1
fi

# Lane concurrency is chosen from the host's CPU count: each lane runs its own
# worker pool, so on a machine too small to cover them the gate serializes
# rather than oversubscribing and starving a pool past its per-test timeout.
# A serialized lane is reaped as it is launched, so it settles through its
# `<step>.rc` file rather than through `wait`; these cases cover that path and
# the decision that selects it.
run_lane_cpu_case() {
  local label="$1"
  local cpus="$2"
  local plan="${3:-parallel}"
  local state="$tmp_root/state-lanecpu-$label"
  local target="$tmp_root/target lanecpu $label"
  local build="$tmp_root/build lanecpu $label"
  mkdir -p "$state" "$target/deps" "$build/debug"
  : > "$state/cargo-record"
  : > "$state/make-record"
  : > "$state/event-record"
  : > "$state/lane-env-record"
  set +e
  HARN_RELEASE_ROOT="$release_root" \
    HARN_BIN="$fake_harn" \
    CARGO_TARGET_DIR="$target" \
    CARGO_BUILD_BUILD_DIR="$build" \
    FAKE_AUDIT_LANE="$plan" \
    HARN_RELEASE_GATE_LANE_CPUS="$cpus" \
    FAKE_CARGO_MODE=success \
    FAKE_CARGO_RECORD="$state/cargo-record" \
    FAKE_CARGO_STATE="$state" \
    FAKE_EVENT_RECORD="$state/event-record" \
    FAKE_LANE_ENV_RECORD="$state/lane-env-record" \
    FAKE_MAKE_MODE=success \
    FAKE_MAKE_RECORD="$state/make-record" \
    PATH="$fake_bin:$PATH" \
    "$release_tools/release_gate.sh" audit --source-only > "$state/output" 2>&1
  local status=$?
  set -e
  printf '%s\n' "$status" > "$state/status"
  printf '%s\n' "$state"
}

# Two lanes on two CPUs cannot give either pool more than one worker.
serial_state=$(run_lane_cpu_case serial 2)
if [[ "$(<"$serial_state/status")" -ne 0 ]]; then
  echo "serialized audit lanes should still pass" >&2
  cat "$serial_state/output" >&2
  exit 1
fi
if ! grep -Fq 'audit lanes: serial (2 cpu for 2 lanes)' "$serial_state/output"; then
  echo "a host too small for its lanes should serialize them" >&2
  cat "$serial_state/output" >&2
  exit 1
fi
# Both lanes must still be reported, which is what proves the `.rc` settle path
# reaches the same bookkeeping as the `wait` path.
for lane in rust-audit security-audit; do
  if ! grep -Eq "^ok: +$lane " "$serial_state/output"; then
    echo "serialized lane $lane is missing from the settle report" >&2
    cat "$serial_state/output" >&2
    exit 1
  fi
done

wide_state=$(run_lane_cpu_case wide 64)
if [[ "$(<"$wide_state/status")" -ne 0 ]]; then
  echo "parallel audit lanes should pass on a wide host" >&2
  cat "$wide_state/output" >&2
  exit 1
fi
if grep -Fq 'audit lanes: serial' "$wide_state/output"; then
  echo "a host with CPUs to spare should keep its lanes parallel" >&2
  cat "$wide_state/output" >&2
  exit 1
fi

# Three internally parallel heavy lanes on a wide host remain concurrent, but
# their configured pools partition the machine instead of each claiming all of
# it. On 18 CPUs that gives both Cargo lanes 6 workers and caps conformance at
# its established maximum of 4.
budgeted_state=$(run_lane_cpu_case budgeted 18 constrained)
if [[ "$(<"$budgeted_state/status")" -ne 0 ]]; then
  echo "bounded parallel audit lanes should pass" >&2
  cat "$budgeted_state/output" >&2
  exit 1
fi
if ! grep -Fq \
  'audit lanes: bounded parallel (18 cpu; 3 heavy lanes; 6 workers per heavy lane)' \
  "$budgeted_state/output"; then
  echo "a wide host should publish its heavy-lane worker partition" >&2
  cat "$budgeted_state/output" >&2
  exit 1
fi
if ! grep -Eq $'^make\tfmt-check\t6\t$' "$budgeted_state/lane-env-record"; then
  echo "rust audit did not receive its Cargo worker budget" >&2
  cat "$budgeted_state/lane-env-record" >&2
  exit 1
fi
if ! grep -Eq $'^make\tconformance\t\t4$' "$budgeted_state/lane-env-record"; then
  echo "harn audit did not receive its bounded conformance worker pool" >&2
  cat "$budgeted_state/lane-env-record" >&2
  exit 1
fi
constrained_state=$(run_lane_cpu_case constrained 6 constrained)
if [[ "$(<"$constrained_state/status")" -ne 0 ]]; then
  echo "resource-aware audit lanes should still pass" >&2
  cat "$constrained_state/output" >&2
  exit 1
fi
if ! grep -Fq \
  'audit lanes: resource-aware (6 cpu; heavy lanes serialized, light lanes parallel)' \
  "$constrained_state/output"; then
  echo "a medium host should serialize only internally parallel lanes" >&2
  cat "$constrained_state/output" >&2
  exit 1
fi

# Missing lane prerequisites must fail before the warm build, not after the
# candidate has paid any Cargo or AOT preparation cost.
mv "$fake_bin/cargo-nextest" "$fake_bin/cargo-nextest.disabled"
missing_tool_state="$tmp_root/state-missing-tool"
mkdir -p "$missing_tool_state"
: > "$missing_tool_state/cargo-record"
: > "$missing_tool_state/make-record"
: > "$missing_tool_state/event-record"
set +e
HARN_RELEASE_ROOT="$release_root" \
  HARN_BIN="$fake_harn" \
  CARGO_TARGET_DIR="$tmp_root/target-missing-tool" \
  CARGO_BUILD_BUILD_DIR="$tmp_root/build-missing-tool" \
  FAKE_AUDIT_LANE=rust \
  FAKE_CARGO_MODE=success \
  FAKE_CARGO_RECORD="$missing_tool_state/cargo-record" \
  FAKE_CARGO_STATE="$missing_tool_state" \
  FAKE_EVENT_RECORD="$missing_tool_state/event-record" \
  FAKE_MAKE_MODE=success \
  FAKE_MAKE_RECORD="$missing_tool_state/make-record" \
  PATH="$fake_bin:/usr/bin:/bin" \
  "$release_tools/release_gate.sh" audit --source-only \
  > "$missing_tool_state/output" 2>&1
missing_tool_status=$?
set -e
mv "$fake_bin/cargo-nextest.disabled" "$fake_bin/cargo-nextest"
if [[ "$missing_tool_status" -eq 0 ]] \
  || ! grep -Fq 'release audit prerequisites missing: cargo-nextest' \
    "$missing_tool_state/output"; then
  echo "missing cargo-nextest should fail during release preflight" >&2
  cat "$missing_tool_state/output" >&2
  exit 1
fi
if [[ -s "$missing_tool_state/cargo-record" || -s "$missing_tool_state/make-record" ]]; then
  echo "missing prerequisite should fail before build preparation" >&2
  cat "$missing_tool_state/cargo-record" >&2
  cat "$missing_tool_state/make-record" >&2
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
tail -n 60 "$ordinary_parallel_state/output" > "$ordinary_parallel_state/output-tail"
if ! grep -Fq '=== RELEASE AUDIT FAILURE RECAP — failing step(s) ===' \
  "$ordinary_parallel_state/output-tail" \
  || ! grep -Fq 'error[E0308]: ordinary audit-lane compiler failure' \
    "$ordinary_parallel_state/output-tail"; then
  echo "ordinary audit cause must remain in the hosted-output tail" >&2
  cat "$ordinary_parallel_state/output-tail" >&2
  exit 1
fi

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
  "$package_target-package-check"$'\t'"$package_target-package-check/build" ]]; then
  echo "package-audit recovery should clean in the package verification Cargo context" >&2
  cat "$package_state/cargo-env-record" >&2
  exit 1
fi
if [[ "$package_target-package-check" == "$package_target"/* ]]; then
  echo "package verification target must not be nested under the main audit target" >&2
  exit 1
fi
if ! grep -Fxq 'clean -p libsqlite3-sys' "$package_state/cargo-record"; then
  echo "package-audit recovery should clean only the implicated package" >&2
  cat "$package_state/cargo-record" >&2
  exit 1
fi

# A lane can die without writing anything an error-text scan would match. The
# package-audit runner is not wrapped in `time_phase`, so there is no unmatched
# sub-step marker either, and selecting failing lanes by log content dropped it
# from the summary entirely (#6102). The summary must name the lane and say how
# it died.
killed_state="$tmp_root/state-killed"
killed_target="$tmp_root/target killed"
killed_build="$tmp_root/build killed"
mkdir -p "$killed_state" "$killed_target" "$killed_build"
: > "$killed_state/cargo-record"
: > "$killed_state/make-record"
set +e
HARN_RELEASE_ROOT="$release_root" \
  HARN_BIN="$fake_harn" \
  CARGO_TARGET_DIR="$killed_target" \
  CARGO_BUILD_BUILD_DIR="$killed_build" \
  FAKE_AUDIT_LANE=package \
  FAKE_CARGO_MODE=stale-then-success \
  FAKE_CARGO_RECORD="$killed_state/cargo-record" \
  FAKE_CARGO_STATE="$killed_state" \
  FAKE_MAKE_RECORD="$killed_state/make-record" \
  FAKE_PACKAGE_MODE=sigkill \
  PATH="$fake_bin:$PATH" \
  "$release_tools/release_gate.sh" audit --source-only > "$killed_state/output" 2>&1
killed_status=$?
set -e
if [[ "$killed_status" -eq 0 ]]; then
  echo "a signal-killed audit lane must fail the gate" >&2
  cat "$killed_state/output" >&2
  exit 1
fi
if ! grep -Fq '>>> package-audit  <<<' "$killed_state/output"; then
  echo "signal-killed lane is missing from the failure summary" >&2
  cat "$killed_state/output" >&2
  exit 1
fi
if ! grep -Fq 'killed by SIGKILL (exit 137)' "$killed_state/output"; then
  echo "failure summary did not report how the lane died" >&2
  cat "$killed_state/output" >&2
  exit 1
fi

# Regression for #6212. A v0.10.55 release cleaned the one package its first
# classification named and then went terminal on a second stale package, because
# recovery had a retry budget of exactly one. Each round must clean what that
# round reveals.
cascading_state=$(run_case cascading cascading)
if [[ "$(<"$cascading_state/status")" -ne 0 ]]; then
  echo "cascading stale packages should recover across rounds" >&2
  cat "$cascading_state/output" >&2
  exit 1
fi
if [[ "$(grep -c '^build -p harn-cli --bin harn --quiet$' "$cascading_state/cargo-record")" -ne 3 ]]; then
  echo "cascading recovery should build three times (initial + two rounds)" >&2
  cat "$cascading_state/cargo-record" >&2
  exit 1
fi
if ! grep -Fxq 'clean -p libsqlite3-sys' "$cascading_state/cargo-record" \
  || ! grep -Fxq 'clean -p tree-sitter' "$cascading_state/cargo-record"; then
  echo "each cascading round should clean only the package that round revealed" >&2
  cat "$cascading_state/cargo-record" >&2
  exit 1
fi
if [[ ! -f "$tmp_root/target cascading/sentinel" ]]; then
  echo "cascading recovery discarded the target dir it did not need to" >&2
  exit 1
fi

# Decay past what per-package classification can reach: the round names only
# packages an earlier round already cleaned, so the cache itself is the problem.
unreachable_state=$(run_case unreachable unreachable)
if [[ "$(<"$unreachable_state/status")" -ne 0 ]]; then
  echo "unreachable stale output should recover by clearing the target dir" >&2
  cat "$unreachable_state/output" >&2
  exit 1
fi
if ! grep -Fq 'recovery: package-scoped cleanup found nothing new for warm prebuild; clearing' \
  "$unreachable_state/output"; then
  echo "whole-target fallback telemetry is missing" >&2
  cat "$unreachable_state/output" >&2
  exit 1
fi
if [[ -f "$tmp_root/target unreachable/sentinel" ]]; then
  echo "whole-target fallback did not actually clear the target dir" >&2
  exit 1
fi

echo "release_gate_stale_out_dir_test: ok"
