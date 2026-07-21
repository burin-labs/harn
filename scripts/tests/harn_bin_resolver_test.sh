#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/harn"
cat > "$fake_bin" <<'SH'
#!/usr/bin/env bash
printf 'fake harn\n'
SH
chmod +x "$fake_bin"

HARN_BIN="$fake_bin" "$repo_root/scripts/harn_bin.sh" --print >"$tmp_root/explicit.out"
if ! grep -Fxq "$fake_bin" "$tmp_root/explicit.out"; then
  echo "harn_bin resolver did not return the explicit executable HARN_BIN" >&2
  cat "$tmp_root/explicit.out" >&2
  exit 1
fi

non_exec="$tmp_root/not-executable"
printf 'not executable\n' > "$non_exec"
if HARN_BIN="$non_exec" "$repo_root/scripts/harn_bin.sh" --print >"$tmp_root/non-exec.out" 2>"$tmp_root/non-exec.err"; then
  echo "harn_bin resolver accepted a non-executable HARN_BIN" >&2
  cat "$tmp_root/non-exec.out" >&2
  exit 1
fi
if ! grep -Fq "harn binary is not executable" "$tmp_root/non-exec.err"; then
  echo "non-executable HARN_BIN error did not explain the validation failure" >&2
  cat "$tmp_root/non-exec.err" >&2
  exit 1
fi

fake_cargo_bin="$tmp_root/fake-cargo-bin"
target_dir="$tmp_root/target dir"
record="$tmp_root/cargo-record.txt"
mkdir -p "$fake_cargo_bin" "$target_dir/debug"
cat > "$fake_cargo_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'args=%s\n' "$*"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-__unset__}"
  printf 'CARGO_BUILD_BUILD_DIR=%s\n' "${CARGO_BUILD_BUILD_DIR-__unset__}"
  printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-__unset__}"
  printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-__unset__}"
} >> "$FAKE_CARGO_RECORD"

case "${FAKE_CARGO_MODE:-success}" in
  ordinary-failure)
    echo "ordinary cargo failure" >&2
    exit 17
    ;;
  wrapper-timeout|wrapper-timeout-retry-failure)
    if [[ -n "${RUSTC_WRAPPER:-}" || -n "${CARGO_BUILD_RUSTC_WRAPPER:-}" ]]; then
      tail -f /dev/null &
      printf '%s\n' "$!" > "${FAKE_CARGO_CHILD_PID_FILE:?}"
      wait "$!"
    fi
    if [[ "$FAKE_CARGO_MODE" = "wrapper-timeout-retry-failure" ]]; then
      echo "retry cargo failure" >&2
      exit 19
    fi
    ;;
esac

case "$*" in
  "run --quiet --bin harn -- __internal-executable-path")
    mkdir -p "${CARGO_TARGET_DIR:?}/debug"
    cat > "$CARGO_TARGET_DIR/debug/harn" <<'BIN'
#!/usr/bin/env bash
set -euo pipefail
printf 'fake harn\n'
BIN
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    printf '%s\n' "$CARGO_TARGET_DIR/debug/harn"
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_cargo_bin/cargo"

unset HARN_BIN

env -u CARGO_TARGET_DIR -u CARGO_BUILD_BUILD_DIR \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print > "$tmp_root/cargo-run.out"
expected_bin="$target_dir/debug/harn"
if ! grep -Fxq "$expected_bin" "$tmp_root/cargo-run.out"; then
  echo "harn_bin resolver did not return Cargo's executable-path probe result" >&2
  cat "$tmp_root/cargo-run.out" >&2
  exit 1
fi
if ! grep -Fxq "args=run --quiet --bin harn -- __internal-executable-path" "$record"; then
  echo "harn_bin resolver did not delegate binary resolution to cargo run" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$target_dir" "$record"; then
  echo "harn_bin resolver did not align Cargo build-dir with CARGO_TARGET_DIR" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --no-build --print > "$tmp_root/no-build.out"
if ! grep -Fxq "$expected_bin" "$tmp_root/no-build.out"; then
  echo "harn_bin --no-build did not return the target-dir executable" >&2
  cat "$tmp_root/no-build.out" >&2
  exit 1
fi
if [[ -s "$record" ]]; then
  echo "harn_bin --no-build invoked cargo" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  HARN_BIN_NO_BUILD=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print > "$tmp_root/env-no-build.out"
if ! grep -Fxq "$expected_bin" "$tmp_root/env-no-build.out"; then
  echo "HARN_BIN_NO_BUILD did not return the target-dir executable" >&2
  cat "$tmp_root/env-no-build.out" >&2
  exit 1
fi
if [[ -s "$record" ]]; then
  echo "HARN_BIN_NO_BUILD invoked cargo" >&2
  cat "$record" >&2
  exit 1
fi

missing_target="$tmp_root/missing-target"
: > "$record"
if CARGO_TARGET_DIR="$missing_target" \
  FAKE_CARGO_RECORD="$record" \
  HARN_BIN_NO_BUILD=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/env-no-build-missing.out" \
  2> "$tmp_root/env-no-build-missing.err"; then
  echo "HARN_BIN_NO_BUILD accepted a missing worktree binary" >&2
  exit 1
fi
if ! grep -Fq "no fresh worktree harn binary found" "$tmp_root/env-no-build-missing.err"; then
  echo "HARN_BIN_NO_BUILD missing-binary error was not attributable" >&2
  cat "$tmp_root/env-no-build-missing.err" >&2
  exit 1
fi
if [[ -s "$record" ]]; then
  echo "HARN_BIN_NO_BUILD invoked cargo while reporting a missing binary" >&2
  cat "$record" >&2
  exit 1
fi

if HARN_BIN_NO_BUILD=typo "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/env-no-build-invalid.out" \
  2> "$tmp_root/env-no-build-invalid.err"; then
  echo "harn_bin accepted an invalid HARN_BIN_NO_BUILD value" >&2
  exit 1
fi
if ! grep -Fq "HARN_BIN_NO_BUILD must be 0 or 1" "$tmp_root/env-no-build-invalid.err"; then
  echo "invalid HARN_BIN_NO_BUILD error was not attributable" >&2
  cat "$tmp_root/env-no-build-invalid.err" >&2
  exit 1
fi

: > "$record"
if CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=ordinary-failure \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/ordinary-failure.out" \
  2> "$tmp_root/ordinary-failure.err"; then
  echo "harn_bin resolver accepted an ordinary Cargo failure" >&2
  exit 1
else
  status=$?
fi
if [[ "$status" -ne 17 ]]; then
  echo "ordinary Cargo failure status changed: expected 17, got $status" >&2
  cat "$tmp_root/ordinary-failure.err" >&2
  exit 1
fi
if [[ "$(grep -Fc 'args=run --quiet --bin harn -- __internal-executable-path' "$record")" -ne 1 ]]; then
  echo "ordinary Cargo failure was retried" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
child_pid_file="$tmp_root/timeout-child.pid"
RUSTC_WRAPPER=sccache \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=wrapper-timeout \
  FAKE_CARGO_CHILD_PID_FILE="$child_pid_file" \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=0.1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/timeout-recovery.out" \
  2> "$tmp_root/timeout-recovery.err"
if ! grep -Fxq "$expected_bin" "$tmp_root/timeout-recovery.out"; then
  echo "harn_bin resolver did not recover from a compiler-wrapper timeout" >&2
  cat "$tmp_root/timeout-recovery.err" >&2
  exit 1
fi
if ! grep -Fq "retrying Cargo harn binary probe with the compiler wrapper disabled" "$tmp_root/timeout-recovery.err"; then
  echo "compiler-wrapper timeout recovery was not attributable" >&2
  cat "$tmp_root/timeout-recovery.err" >&2
  exit 1
fi
if [[ "$(grep -Fc 'args=run --quiet --bin harn -- __internal-executable-path' "$record")" -ne 2 ]]; then
  echo "compiler-wrapper timeout did not run exactly one retry" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "RUSTC_WRAPPER=" "$record"; then
  echo "compiler-wrapper timeout retry did not clear RUSTC_WRAPPER" >&2
  cat "$record" >&2
  exit 1
fi
timed_out_child_pid=$(cat "$child_pid_file")
if kill -0 "$timed_out_child_pid" 2>/dev/null; then
  echo "compiler-wrapper timeout left a descendant process alive: $timed_out_child_pid" >&2
  exit 1
fi

: > "$record"
child_pid_file="$tmp_root/retry-failure-child.pid"
if RUSTC_WRAPPER=sccache \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=wrapper-timeout-retry-failure \
  FAKE_CARGO_CHILD_PID_FILE="$child_pid_file" \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=0.1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/retry-failure.out" \
  2> "$tmp_root/retry-failure.err"; then
  echo "harn_bin resolver accepted a failed wrapper-disabled retry" >&2
  exit 1
else
  status=$?
fi
if [[ "$status" -ne 19 ]]; then
  echo "wrapper-disabled retry failure status changed: expected 19, got $status" >&2
  cat "$tmp_root/retry-failure.err" >&2
  exit 1
fi

if HARN_BIN_CARGO_TIMEOUT_SECONDS=invalid \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/invalid-timeout.out" \
  2> "$tmp_root/invalid-timeout.err"; then
  echo "harn_bin resolver accepted an invalid Cargo probe timeout" >&2
  exit 1
else
  status=$?
fi
if [[ "$status" -ne 2 ]] || ! grep -Fq "must be a positive number" "$tmp_root/invalid-timeout.err"; then
  echo "invalid Cargo probe timeout was not reported with status 2" >&2
  cat "$tmp_root/invalid-timeout.err" >&2
  exit 1
fi

echo "harn_bin_resolver_test: ok"
