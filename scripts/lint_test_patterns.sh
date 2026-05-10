#!/usr/bin/env bash
#
# Lint script that fails if wall-clock polling patterns appear in test files
# outside an explicit per-pattern allowlist. Part of the deflake epic (#1057).
#
# These patterns cause flaky tests because they race against scheduler
# behavior and system load. The approved replacements use injected time
# (tokio::time::pause/advance), event subscriptions, or deterministic
# harnesses.
#
# Usage: ./scripts/lint_test_patterns.sh
#   Exits 0 if no violations found, 1 otherwise.
#
# To suppress a false-positive, add the file to the appropriate per-pattern
# allowlist below with a brief comment explaining why. All suppressions are
# pre-existing technical debt tracked under the deflake epic; new entries
# require explicit reviewer justification.
#
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# ---------------------------------------------------------------------------
# Per-pattern allowlists — pre-existing violations from before the deflake
# epic. Remove entries as each tier of #1057 completes.
# ---------------------------------------------------------------------------

# std::thread::sleep in test files
# Legitimate uses: waiting on real subprocess I/O that cannot be time-paused.
THREAD_SLEEP_ALLOWLIST=(
  "crates/harn-hostlib/tests/process_tools.rs"
)

# tokio::time::sleep in test files (outside start_paused = true tests).
# Legitimate uses:
#   - `orchestration/tests.rs`: paired with `start_paused = true`,
#     drives advanced timers explicitly.
#   - `orchestrator_http/support.rs`: `wait_for_path` polls a real
#     filesystem marker written by handler-side code where no
#     event signal is available; bounded with `tokio::time::timeout`.
TOKIO_SLEEP_ALLOWLIST=(
  "crates/harn-vm/src/orchestration/tests.rs"
  "crates/harn-cli/tests/orchestrator_http/support.rs"
)

# Wall-clock `Instant::now() < deadline` comparisons inside any loop or
# guard.
INSTANT_NOW_DEADLINE_ALLOWLIST=(
)

# SystemTime::now() in test files — use injected clock / MockClock instead.
# Remaining entries: legitimate fixture-setup uses against real OS time
# (httpdate Retry-After parsing, real-mtime touch fixtures, tempdir
# nano-suffix uniqueness).
SYSTEM_TIME_ALLOWLIST=(
  "crates/harn-vm/src/http/tests.rs"
  "crates/harn-hostlib/tests/scanner_e2e.rs"
  "crates/harn-cli/src/commands/check/tests.rs"
)

# recv_timeout with an explicit sub-second Duration literal.
# Named-constant variants are not caught here because the constant value
# cannot be resolved by a static text search.
RECV_TIMEOUT_MILLIS_ALLOWLIST=()

# Conformance tests that exercise real subprocesses should share the bounded
# polling helpers from `conformance/tests/_common.harn` instead of copying
# ad hoc loops into each fixture.
CONFORMANCE_HELPER_ALLOWLIST=(
  "conformance/tests/_common.harn"
)

# Conformance fixtures that are allowed to call `sleep(<literal>)` /
# `time.sleep(<literal>)` because they exercise real subprocess I/O,
# real socket-bound servers, or genuine wall-clock-driven scheduler
# behavior that cannot be expressed under `mock_time(...)`. New
# fixtures should drive timing through `mock_time(...)` /
# `advance_time(...)` and `yield_now()` from the unified test clock
# (see docs/src/dev/testing.md). Add an entry here only when there is
# no deterministic alternative — and prefer shrinking this list over
# growing it.
CONFORMANCE_REAL_TIME_ALLOWLIST=(
  "conformance/tests/_common.harn"
  "conformance/tests/agents/daemon_stdlib_wrappers.harn"
  "conformance/tests/agents/worker_retriggerable.harn"
  "conformance/tests/agents/workflow_messages.harn"
  "conformance/tests/concurrency/deadline_catch.harn"
  "conformance/tests/concurrency/parallel_each_as_stream.harn"
  "conformance/tests/concurrency/parallel_max_concurrent.harn"
  "conformance/tests/concurrency/parallel_race.harn"
  "conformance/tests/concurrency/rwlock_channel_select.harn"
  "conformance/tests/concurrency/select_basic.harn"
  "conformance/tests/concurrency/supervisor_circuit_open.harn"
  "conformance/tests/concurrency/supervisor_graceful_stop.harn"
  "conformance/tests/concurrency/supervisor_one_for_one.harn"
  "conformance/tests/concurrency/supervisor_restart_cap.harn"
  "conformance/tests/concurrency/supervisor_restart_window.harn"
  "conformance/tests/integration/agent_loop_mcp_http_elicit.harn"
  "conformance/tests/integration/agent_loop_mcp_servers.harn"
  "conformance/tests/integration/orchestrator_hot_reload_add.harn"
  "conformance/tests/integration/orchestrator_hot_reload_modify_inflight.harn"
  "conformance/tests/integration/orchestrator_hot_reload_remove.harn"
  "conformance/tests/integration/orchestrator_pump_drain_lifecycle.harn"
  "conformance/tests/integration/orchestrator_recover_stranded_envelopes.harn"
  "conformance/tests/integration/orchestrator_worker_claim_expiry_requeue.harn"
  "conformance/tests/runtime/http_proxy_passthrough.harn"
  "conformance/tests/stdlib/hitl_pending.harn"
  "conformance/tests/stdlib/waitpoint_fan_out.harn"
  "conformance/tests/stdlib/waitpoints_fan_out.harn"
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

violations=0

# Check if a path is in a given allowlist (bash array passed by name).
in_allowlist() {
  local path="$1"
  local -n _list="$2"
  # Normalize to a path relative to the repo root for comparison.
  local rel="${path#"$ROOT_DIR/"}"
  for entry in "${_list[@]}"; do
    if [[ "$rel" == "$entry" ]]; then
      return 0
    fi
  done
  return 1
}

is_e2e_subprocess_path() {
  local path="$1"
  local rel="${path#"$ROOT_DIR/"}"
  case "$rel" in
    crates/harn-cli/tests/acp_server_cli.rs | \
    crates/harn-cli/tests/burin_mini_playground.rs | \
    crates/harn-cli/tests/flow_ship_cli.rs | \
    crates/harn-cli/tests/harn_serve_mcp_cli.rs | \
    crates/harn-cli/tests/llm_mock_cli.rs | \
    crates/harn-cli/tests/mcp_server_cli.rs | \
    crates/harn-cli/tests/orchestrator_cli.rs | \
    crates/harn-cli/tests/persona_cli.rs | \
    crates/harn-cli/tests/run_eval_imports.rs | \
    crates/harn-cli/tests/run_exit_codes.rs | \
    crates/harn-cli/tests/trigger_replay_cli.rs | \
    crates/harn-cli/tests/orchestrator_http.rs | \
    crates/harn-cli/tests/orchestrator_http/* | \
    crates/harn-cli/tests/support/mcp.rs | \
    crates/harn-cli/tests/support/process.rs | \
    crates/harn-cli/tests/test_util/process.rs)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

# Collect test file paths into a temp file so we can reuse the list.
TEST_FILES_TMP="$(mktemp)"
HARN_TEST_FILES_TMP="$(mktemp)"
trap 'rm -f "$TEST_FILES_TMP" "$HARN_TEST_FILES_TMP"' EXIT

{
  # crates/**/tests/**/*.rs — any depth under a tests/ directory
  find crates -type f -name "*.rs" -path "*/tests/*"
  # crates/**/src/**/tests.rs and tests_*.rs — inline test modules
  find crates -type f \( -name "tests.rs" -o -name "tests_*.rs" \) -path "*/src/*"
} | sort -u > "$TEST_FILES_TMP"

find conformance/tests -type f -name "*.harn" | sort -u > "$HARN_TEST_FILES_TMP"

# check_pattern PATTERN ALLOWLIST_VAR SUGGESTION
#   Greps PATTERN across test files, skipping allowlisted files.
#   Prints file:line for each violation and increments $violations.
check_pattern() {
  local pattern="$1"
  local allowlist_var="$2"
  local suggestion="$3"

  while IFS= read -r file; do
    in_allowlist "$file" "$allowlist_var" && continue
    while IFS= read -r hit; do
      [[ -z "$hit" ]] && continue
      echo "  $hit"
      echo "    hint: $suggestion"
      violations=$((violations + 1))
    done < <(grep -n -- "$pattern" "$file" 2>/dev/null || true)
  done < "$TEST_FILES_TMP"
}

# ---------------------------------------------------------------------------
# Pattern checks
# ---------------------------------------------------------------------------

echo "=== Checking test files for wall-clock patterns ==="

echo "--- std::thread::sleep ---"
check_pattern \
  "std::thread::sleep\|thread::sleep(Duration\|thread::sleep(std::time" \
  THREAD_SLEEP_ALLOWLIST \
  "Use tokio::time::pause() + advance(), or wait on an event/channel instead."

echo "--- tokio::time::sleep ---"
check_pattern \
  "tokio::time::sleep(" \
  TOKIO_SLEEP_ALLOWLIST \
  "Use tokio::time::pause() + advance() in a #[tokio::test(start_paused = true)] context."

echo "--- Instant::now() deadline comparison (wall-clock poll) ---"
# Catches `Instant::now() < deadline`, `>= deadline`, etc. — the smell is
# the comparison itself, regardless of whether it sits inside a `while`,
# a `loop`, or an `assert!` macro inside a loop body.
check_pattern \
  "Instant::now() *[<>]" \
  INSTANT_NOW_DEADLINE_ALLOWLIST \
  "Subscribe to an EventLog channel, use OrchestratorHarness, or wrap a poll in tokio::time::timeout instead."

echo "--- SystemTime::now() in tests ---"
check_pattern \
  "SystemTime::now()\|std::time::SystemTime::now()" \
  SYSTEM_TIME_ALLOWLIST \
  "Inject a MockClock or use the stdlib clock() builtin via a test harness."

echo "--- recv_timeout with explicit sub-second Duration ---"
check_pattern \
  "recv_timeout(Duration::from_millis\|recv_timeout(Duration::from_nanos\|recv_timeout(Duration::from_micros" \
  RECV_TIMEOUT_MILLIS_ALLOWLIST \
  "Use an event-driven wait (channel recv with tokio::time::timeout) instead of busy-polling."

echo "--- obsolete harn_command helper ---"
while IFS= read -r file; do
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    echo "  $hit"
    echo "    hint: harn_command was retired; use in-process APIs in fast tests or harn_e2e_command in the E2E suite."
    violations=$((violations + 1))
  done < <(grep -n "harn_command(" "$file" 2>/dev/null || true)
done < "$TEST_FILES_TMP"

echo "--- harn_e2e_command outside E2E subprocess tests ---"
while IFS= read -r file; do
  is_e2e_subprocess_path "$file" && continue
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    echo "  $hit"
    echo "    hint: harn_e2e_command is E2E-only; use in-process command/library APIs in fast tests."
    violations=$((violations + 1))
  done < <(grep -n "harn_e2e_command(" "$file" 2>/dev/null || true)
done < "$TEST_FILES_TMP"

echo "--- spawn_orchestrator outside E2E subprocess tests ---"
while IFS= read -r file; do
  is_e2e_subprocess_path "$file" && continue
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    echo "  $hit"
    echo "    hint: spawn_orchestrator is E2E-only; use OrchestratorHarness in fast tests."
    violations=$((violations + 1))
  done < <(grep -n "spawn_orchestrator(" "$file" 2>/dev/null || true)
done < "$TEST_FILES_TMP"

echo "--- copied conformance subprocess wait helpers ---"
check_harn_helper_pattern() {
  local pattern="$1"
  local suggestion="$2"

  while IFS= read -r file; do
    in_allowlist "$file" CONFORMANCE_HELPER_ALLOWLIST && continue
    while IFS= read -r hit; do
      [[ -z "$hit" ]] && continue
      echo "  $hit"
      echo "    hint: $suggestion"
      violations=$((violations + 1))
    done < <(grep -n -- "$pattern" "$file" 2>/dev/null || true)
  done < "$HARN_TEST_FILES_TMP"
}

check_harn_helper_pattern \
  "^fn wait_for_listener_url\|^fn wait_for_a2a_server_url\|^fn wait_for_log_line\|^fn wait_for_exit" \
  "Import the shared helper from conformance/tests/_common.harn so retry ceilings stay consistent."

echo "--- random fixed-port conformance server allocation ---"
while IFS= read -r file; do
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    echo "  $hit"
    echo "    hint: Bind test servers to port 0 and read the selected port from the readiness log."
    violations=$((violations + 1))
  done < <(grep -n "random_int(20000, 45000)" "$file" 2>/dev/null || true)
done < "$HARN_TEST_FILES_TMP"

echo "--- conformance sleep with fixed literal duration ---"
# Fixtures that drive timing via `mock_time(...)` / `advance_time(...)`
# auto-deflake their `sleep(...)` calls (the unified test clock advances
# instantly). Anything else relies on wall-clock and should either move
# to that pattern or be added to CONFORMANCE_REAL_TIME_ALLOWLIST with
# justification.
while IFS= read -r file; do
  in_allowlist "$file" CONFORMANCE_REAL_TIME_ALLOWLIST && continue
  # Files that install a clock mock are exempt because `sleep(...)` and
  # `sleep_ms(...)` advance the mock instead of suspending the runtime.
  if grep -q "^[[:space:]]*mock_time(" "$file" 2>/dev/null; then
    continue
  fi
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    echo "  $hit"
    echo "    hint: Wrap timing-sensitive logic in mock_time(...)/unmock_time(),"
    echo "          or add the fixture to CONFORMANCE_REAL_TIME_ALLOWLIST in"
    echo "          scripts/lint_test_patterns.sh with reviewer justification."
    violations=$((violations + 1))
  done < <(grep -n -E "(^|[^_a-zA-Z])(sleep|time\.sleep)\([0-9]" "$file" 2>/dev/null || true)
done < "$HARN_TEST_FILES_TMP"

# ---------------------------------------------------------------------------
echo
if (( violations > 0 )); then
  echo "FAIL: $violations wall-clock pattern violation(s) found in test files."
  echo
  echo "Forbidden patterns cause flaky tests. See docs/src/dev/testing.md for"
  echo "approved alternatives and how to opt out with reviewer justification."
  exit 1
else
  echo "OK: no wall-clock pattern violations found."
fi
