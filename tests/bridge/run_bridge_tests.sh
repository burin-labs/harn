#!/bin/bash
# Integration tests for harn --bridge mode using the Python mock host.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
HARN="$REPO_ROOT/target/debug/harn"
MOCK_HOST="$SCRIPT_DIR/../bridge_mock_host.py"

# Build first
echo "Building harn..."
cargo build --bin harn --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null

PASSED=0
FAILED=0

run_test() {
    local name="$1"
    local pipeline="$2"
    local expected="$3"
    shift 3

    local actual
    actual=$(python3 "$MOCK_HOST" "$HARN" "$pipeline" "$@" 2>/dev/null) || true

    if echo "$actual" | grep -qF "$expected"; then
        echo "  PASS  $name"
        PASSED=$((PASSED + 1))
    else
        echo "  FAIL  $name"
        echo "    expected: $expected"
        echo "    actual:   $actual"
        FAILED=$((FAILED + 1))
    fi
}

echo ""
echo "=== Bridge Integration Tests ==="
echo ""

run_test "llm_call delegation" \
    "$SCRIPT_DIR/test_llm_call.harn" \
    "Mock LLM response to: What is 2+2?"

run_test "read_file delegation" \
    "$SCRIPT_DIR/test_read_file.harn" \
    "Mock content of src/main.rs"

run_test "host_call delegation" \
    "$SCRIPT_DIR/test_host_call.harn" \
    "Mock host_call result for: get_system_prompt"

run_test "task argument passing" \
    "$SCRIPT_DIR/test_task_arg.harn" \
    "Task: Fix the bug" \
    --arg '{"task":"Fix the bug"}'

run_test "combined bridge calls" \
    "$SCRIPT_DIR/test_combined.harn" \
    "Success"

echo ""
echo "$PASSED passed, $FAILED failed, $((PASSED + FAILED)) total"

if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
