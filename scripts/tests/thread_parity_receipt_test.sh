#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
parser="$repo_root/scripts/ci/summarize_nextest_receipt.mjs"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

cat >"$tmpdir/inventory.json" <<'JSON'
{
  "test-count": 3,
  "rust-suites": {
    "demo::tests": {
      "package-name": "demo",
      "binary-name": "tests",
      "status": "listed",
      "testcases": {
        "passes": {"filter-match": {"status": "matches"}},
        "omitted": {"filter-match": {"status": "matches"}},
        "ignored": {"filter-match": {"status": "mismatch", "reason": "ignored"}}
      }
    }
  }
}
JSON

cat >"$tmpdir/complete.jsonl" <<'JSONL'
{"type":"test","event":"started","name":"demo::tests$passes"}
{"type":"test","event":"ok","name":"demo::tests$passes"}
{"type":"test","event":"started","name":"demo::tests$omitted"}
{"type":"test","event":"ok","name":"demo::tests$omitted"}
JSONL

node "$parser" --inventory "$tmpdir/inventory.json" --events "$tmpdir/complete.jsonl" \
  --runner-status 0 --threads 16 >"$tmpdir/complete.log"
grep -q 'reason=complete threads=16 selected=2 run=2 passed=2 skipped=1 not_run=0 failed=0 runner_status=0' \
  "$tmpdir/complete.log"

head -n 2 "$tmpdir/complete.jsonl" >"$tmpdir/incomplete.jsonl"
set +e
node "$parser" --inventory "$tmpdir/inventory.json" --events "$tmpdir/incomplete.jsonl" \
  --runner-status 100 --threads 16 >"$tmpdir/incomplete.log" 2>"$tmpdir/incomplete.err"
status=$?
set -e
[[ "$status" -eq 1 ]]
grep -q 'reason=tests-not-run threads=16 selected=2 run=1 passed=1 skipped=1 not_run=1 failed=0 runner_status=100' \
  "$tmpdir/incomplete.log"
grep -Fqx 'thread_parity_missing_test demo::tests$omitted' "$tmpdir/incomplete.err"

cat >"$tmpdir/failure.jsonl" <<'JSONL'
{"type":"test","event":"failed","name":"demo::tests$passes","exec_time":0.1}
{"type":"test","event":"ok","name":"demo::tests$omitted","exec_time":0.1}
JSONL
set +e
node "$parser" --inventory "$tmpdir/inventory.json" --events "$tmpdir/failure.jsonl" \
  --runner-status 100 --threads 8 >"$tmpdir/failure.log"
status=$?
set -e
[[ "$status" -eq 1 ]]
grep -q 'reason=test-failure threads=8 selected=2 run=2 passed=1 skipped=1 not_run=0 failed=1 runner_status=100' \
  "$tmpdir/failure.log"

set +e
node "$parser" --inventory "$tmpdir/inventory.json" --events "$tmpdir/complete.jsonl" \
  --runner-status 70 --threads 1 >"$tmpdir/runner-error.log"
status=$?
set -e
[[ "$status" -eq 1 ]]
grep -q 'reason=runner-error threads=1 selected=2 run=2 passed=2 skipped=1 not_run=0 failed=0 runner_status=70' \
  "$tmpdir/runner-error.log"

cat >"$tmpdir/empty.json" <<'JSON'
{"test-count": 0, "rust-suites": {}}
JSON
set +e
node "$parser" --inventory "$tmpdir/empty.json" --events /dev/null \
  --runner-status 0 --threads 16 >"$tmpdir/empty.log" 2>&1
status=$?
set -e
[[ "$status" -eq 2 ]]
grep -q 'reason=receipt-invalid.*inventory selected zero tests' "$tmpdir/empty.log"

sed 's/"test-count": 3/"test-count": 4/' "$tmpdir/inventory.json" >"$tmpdir/bad-count.json"
set +e
node "$parser" --inventory "$tmpdir/bad-count.json" --events "$tmpdir/complete.jsonl" \
  --runner-status 0 --threads 16 >"$tmpdir/bad-count.log" 2>&1
status=$?
set -e
[[ "$status" -eq 2 ]]
grep -q 'reason=receipt-invalid.*does not equal selected+skipped' "$tmpdir/bad-count.log"

grep -q 'scripts/ci/run_thread_parity.sh.*matrix.threads' "$repo_root/.github/workflows/thread-parity.yml"
grep -q -- '--message-format libtest-json-plus' "$repo_root/scripts/ci/run_thread_parity.sh"
grep -q -- '--message-format-version 0.1' "$repo_root/scripts/ci/run_thread_parity.sh"
grep -q 'RUST_TEST_STDOUT_PATH' "$repo_root/scripts/ci/run_thread_parity.sh"

heavy_line="$(grep -n 'test-group = "harn-cli-heavy-replay"' "$repo_root/.config/nextest.toml" | cut -d: -f1)"
broad_line="$(grep -n 'test-group = "harn-subprocess"' "$repo_root/.config/nextest.toml" | head -n 1 | cut -d: -f1)"
[[ "$heavy_line" -lt "$broad_line" ]]

echo "thread parity receipt tests: ok"
