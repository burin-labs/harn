#!/usr/bin/env bash
# Cross-platform release smoke driver for the macOS/Linux/Windows matrix.
#
# Exercises the user-visible CLI surface against a pre-built `harn`
# binary and reports each capability check as a separate GitHub Actions
# log group. A failure surfaces as `::error::<platform>:<step> failed`
# so the smoke matrix points directly at the platform and capability
# that regressed. See docs/src/dev/platform-compatibility.md for the
# support matrix and rationale per capability.
#
# Invoked from `.github/workflows/release-smoke.yml` and from
# `make release-smoke` for local reproduction. Override the binary
# path with HARN_BINARY=/path/to/harn.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# Detect the platform tag once. uname is available everywhere we run
# (Git Bash on Windows ships it). Fold the long Cygwin/MinGW/MSYS
# strings into "windows" so step labels stay readable.
case "$(uname -s 2>/dev/null || echo unknown)" in
  Darwin*) PLATFORM="macos" ;;
  Linux*) PLATFORM="linux" ;;
  MINGW*|MSYS*|CYGWIN*) PLATFORM="windows" ;;
  *) PLATFORM="unknown" ;;
esac

EXE_SUFFIX=""
if [[ "$PLATFORM" == "windows" ]]; then
  EXE_SUFFIX=".exe"
fi

HARN="${HARN_BINARY:-target/release/harn${EXE_SUFFIX}}"
if [[ ! -x "$HARN" ]]; then
  echo "::error::release-smoke: harn binary not found at $HARN; build with 'cargo build --release -p harn-cli' or set HARN_BINARY"
  exit 1
fi

STEP_TIMEOUT_SECONDS="${HARN_RELEASE_SMOKE_STEP_TIMEOUT_SECONDS:-120}"
if [[ ! "$STEP_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "::error::release-smoke: HARN_RELEASE_SMOKE_STEP_TIMEOUT_SECONDS must be a positive integer, got '$STEP_TIMEOUT_SECONDS'"
  exit 1
fi

# Disable real LLM dispatch even though every fixture uses
# `provider: "mock"`. A misrouted call would otherwise pull on the
# host-credential path and obscure the smoke failure mode.
export HARN_LLM_CALLS_DISABLED=1

# Per-invocation temp root so concurrent matrix shards on the same
# runner cache do not collide. `mktemp -d` is portable across macOS,
# Linux, and Git Bash on Windows.
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

FAILED=0

terminate_pid_tree() {
  local pid="$1"
  if [[ "$PLATFORM" == "windows" ]]; then
    taskkill //F //T //PID "$pid" >/dev/null 2>&1 || true
  else
    kill "$pid" 2>/dev/null || true
    sleep 2
    kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
  fi
}

run_with_timeout() {
  local timeout_seconds="$1"
  shift
  "$@" &
  local cmd_pid=$!
  local elapsed=0
  while kill -0 "$cmd_pid" 2>/dev/null; do
    if [[ "$elapsed" -ge "$timeout_seconds" ]]; then
      echo "release-smoke ($PLATFORM): command exceeded ${timeout_seconds}s; terminating"
      terminate_pid_tree "$cmd_pid"
      wait "$cmd_pid" 2>/dev/null || true
      return 124
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  local rc=0
  wait "$cmd_pid" || rc=$?
  return "$rc"
}

# Run one labelled step. stdout streams to the log group; on failure
# we emit a `::error::` annotation so the PR summary points at the
# specific (platform, capability) regression instead of just "smoke
# matrix failed". Capture an explicit exit code rather than letting
# `set -e` abort — the loop continues so one regression does not mask
# a second one.
run_step() {
  local label="$1"
  shift
  echo "::group::[$PLATFORM] $label"
  local start_seconds=$SECONDS
  local rc=0
  run_with_timeout "$STEP_TIMEOUT_SECONDS" "$@" || rc=$?
  local elapsed_seconds=$((SECONDS - start_seconds))
  echo "release-smoke ($PLATFORM): $label finished in ${elapsed_seconds}s"
  echo "::endgroup::"
  if [[ "$rc" -ne 0 ]]; then
    if [[ "$rc" -eq 124 ]]; then
      echo "::error::release-smoke ($PLATFORM): $label timed out after ${STEP_TIMEOUT_SECONDS}s"
    else
      echo "::error::release-smoke ($PLATFORM): $label failed (exit $rc)"
    fi
    FAILED=1
  fi
}

smoke_help() {
  # Suppress the multi-page help body so the smoke log stays scannable;
  # the exit code still tells us whether clap wired up cleanly.
  "$HARN" --help >/dev/null
}

smoke_provider_matrix() {
  "$HARN" check --provider-matrix --format markdown >"$TMP_ROOT/provider-matrix.md"
}

# `harn watch` runs forever, so the smoke boots it in the background,
# polls for the "watching ... for .harn changes" status line, then
# terminates the child. Catches `notify` backend regressions per
# platform (FSEvents on macOS, inotify on Linux, ReadDirectoryChangesW
# on Windows). Sandbox/signal-driven shutdown is not exercised here —
# that lives in the orchestrator-* test modules and is documented as
# Unix-only in docs/src/dev/windows-test-coverage.md.
smoke_watch_boot() {
  local watch_log="$TMP_ROOT/watch.log"
  local watch_target="$TMP_ROOT/watch_target.harn"
  printf 'pipeline default() {\n  println("watch boot")\n}\n' >"$watch_target"
  "$HARN" watch "$watch_target" >"$watch_log" 2>&1 &
  local watch_pid=$!
  # 30 iterations × 0.5 s = 15 s timeout. Generous on a cold runner
  # without depending on `timeout(1)`, which is absent from BSD
  # userland on macOS unless coreutils is installed.
  local ready=0
  for _ in $(seq 1 30); do
    if grep -q 'watching .* for .harn changes' "$watch_log" 2>/dev/null; then
      ready=1
      break
    fi
    if ! kill -0 "$watch_pid" 2>/dev/null; then
      break
    fi
    sleep 0.5
  done
  if [[ "$PLATFORM" == "windows" ]]; then
    # Git Bash translates `kill` to a Cygwin signal that harn.exe does
    # not catch; taskkill is the portable forceful shutdown.
    taskkill //F //PID "$watch_pid" >/dev/null 2>&1 || true
  else
    kill "$watch_pid" 2>/dev/null || true
  fi
  wait "$watch_pid" 2>/dev/null || true
  if [[ "$ready" -ne 1 ]]; then
    echo "harn watch did not reach the ready state on $PLATFORM"
    echo "--- watch log ---"
    cat "$watch_log" 2>/dev/null || true
    return 1
  fi
}

# Step 1: version banner. Exercises argument parsing and the binary's
# self-identification. Forks early when the binary cannot start at all
# (missing dynamic library, ABI mismatch).
run_step "harn --version" "$HARN" --version

# Step 2: help text. Confirms the clap subcommand graph wires up on
# this platform. A missing subcommand surfaces here before any heavier
# check exercises it.
run_step "harn --help" smoke_help

# Step 3: type-check the smoke entry point. Catches lexer/parser/type
# regressions on a deterministic LF-only fixture.
run_step "harn check tests/smoke/hello.harn" "$HARN" check tests/smoke/hello.harn

# Step 4: format check. Catches `harn fmt --check` parser drift on the
# same LF-only fixture; fmt churn between platforms is a common
# source of release-time noise.
run_step "harn fmt --check tests/smoke" "$HARN" fmt --check tests/smoke

# Step 5: package check. Validates the harn.toml manifest, exports
# resolution, and path normalization for the smoke package.
run_step "harn package check tests/smoke" "$HARN" package check tests/smoke

# Step 6: generated artifact check. Re-runs the provider matrix
# emitter that `make check-provider-matrix` consumes. Any platform
# divergence in deterministic-text emission (line endings, sort order,
# rounding) lights up here before the release tag is cut.
run_step "harn check --provider-matrix" smoke_provider_matrix

# Step 7: hello-world `harn run`. Confirms the VM, stdlib boot path,
# and `println` host call work end-to-end on this platform.
run_step "harn run tests/smoke/hello.harn" "$HARN" run tests/smoke/hello.harn

# Step 8: process tool. Spawns one short-lived child via std/command
# with platform-appropriate argv. Confirms the sandboxed process
# spawn path resolves on every platform — Seatbelt on macOS, Landlock
# +seccomp on Linux, AppContainer/Job Objects on Windows. Failures
# here usually mean the sandbox backend regressed for one platform.
run_step "harn run tests/smoke/process.harn" "$HARN" run tests/smoke/process.harn

# Step 9: no-credentials workflow. Drives the LLM call path through
# `provider: "mock"` so we touch the agent runtime without API keys,
# secret stores, or network access.
run_step "harn run tests/smoke/mock_workflow.harn" "$HARN" run tests/smoke/mock_workflow.harn

# Step 10: file-watch boot. See smoke_watch_boot above.
run_step "harn watch boot" smoke_watch_boot

if [[ "$FAILED" -ne 0 ]]; then
  echo "::error::release-smoke ($PLATFORM): one or more capabilities regressed"
  exit 1
fi
echo "release-smoke ($PLATFORM): all capabilities ok"
