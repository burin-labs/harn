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
  "crates/harn-cli/tests/test_util/process.rs"
)

# tokio::time::sleep in test files (outside start_paused = true tests)
# Legitimate uses: connector integration tests polling real async state.
TOKIO_SLEEP_ALLOWLIST=(
  "crates/harn-vm/src/connectors/notion/tests.rs"
  "crates/harn-vm/src/connectors/linear/tests.rs"
  "crates/harn-vm/src/triggers/dispatcher/tests/retry.rs"
  "crates/harn-vm/src/orchestration/tests.rs"
  "crates/harn-cli/tests/orchestrator_cli.rs"
  "crates/harn-cli/tests/orchestrator_http/admin.rs"
)

# Instant::now() in a while-loop condition — wall-clock polling.
# All entries below are subprocess/orchestrator integration tests that poll
# real process output; they will be refactored by Tier 1A/1B of #1057.
INSTANT_NOW_WHILE_ALLOWLIST=(
  "crates/harn-vm/src/triggers/dispatcher/tests/retry.rs"
  "crates/harn-cli/tests/orchestrator_cli.rs"
  "crates/harn-cli/tests/orchestrator_inbox_dedupe.rs"
  "crates/harn-cli/tests/orchestrator_http/support.rs"
  "crates/harn-cli/tests/orchestrator_http/admin.rs"
  "crates/harn-cli/tests/orchestrator_http/observability.rs"
  "crates/harn-cli/tests/harn_serve_mcp_cli.rs"
  "crates/harn-cli/tests/support/mcp.rs"
  "crates/harn-cli/tests/support/mod.rs"
  "crates/harn-cli/tests/test_util/process.rs"
)

# SystemTime::now() in test files — use injected clock / MockClock instead.
# Remaining entries: legitimate fixture-setup uses against real OS time
# (httpdate Retry-After parsing, real-mtime touch fixtures, tempdir
# nano-suffix uniqueness, prompt-template `now_ms()` round-trip).
SYSTEM_TIME_ALLOWLIST=(
  "crates/harn-vm/src/http/tests.rs"
  "crates/harn-vm/src/stdlib/template/tests.rs"
  "crates/harn-hostlib/tests/scanner_e2e.rs"
  "crates/harn-cli/src/commands/check/tests.rs"
)

# recv_timeout with an explicit sub-second Duration literal.
# Named-constant variants (for example MCP_LOG_RECV_INTERVAL) are not caught here
# because the constant value cannot be resolved by a static text search.
RECV_TIMEOUT_MILLIS_ALLOWLIST=(
  "crates/harn-cli/tests/harn_serve_mcp_cli.rs"
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
trap 'rm -f "$TEST_FILES_TMP"' EXIT

{
  # crates/**/tests/**/*.rs — any depth under a tests/ directory
  find crates -type f -name "*.rs" -path "*/tests/*"
  # crates/**/src/**/tests.rs and tests_*.rs — inline test modules
  find crates -type f \( -name "tests.rs" -o -name "tests_*.rs" \) -path "*/src/*"
} | sort -u > "$TEST_FILES_TMP"

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

echo "--- Instant::now() in while-loop (wall-clock poll) ---"
check_pattern \
  "while.*Instant::now\|while.*std::time::Instant::now" \
  INSTANT_NOW_WHILE_ALLOWLIST \
  "Subscribe to an EventLog channel or use a deterministic OrchestratorHarness instead."

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

# ---------------------------------------------------------------------------
echo
if (( violations > 0 )); then
  echo "FAIL: $violations wall-clock pattern violation(s) found in test files."
  echo
  echo "Forbidden patterns cause flaky tests. See docs/dev/testing.md for"
  echo "approved alternatives and how to opt out with reviewer justification."
  exit 1
else
  echo "OK: no wall-clock pattern violations found."
fi
