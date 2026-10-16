#!/usr/bin/env bash
set -euo pipefail

: "${HARN_BIN:?run with an already-built Harn executable}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/harn-performance-evidence.XXXXXX")"
trap 'status=$?; if [[ "$status" -eq 0 ]]; then rm -rf "$scratch"; else echo "preserved failed controls: $scratch" >&2; fi' EXIT

cat >"$scratch/measured-harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == version ]]; then
  printf '%s\n' '{"schemaVersion":2,"ok":true,"data":{"source_revision":"fixture-source"}}'
  exit 0
fi
[[ -d "${HARN_CACHE_DIR:?}" && -d "$HOME" ]] || { echo 'missing isolated home/cache' >&2; exit 24; }
if [[ "${PERFORMANCE_FIXTURE_MODE:?}" == child-failure ]]; then
  echo 'fixture child refused' >&2
  exit 23
fi
execute=1
if [[ "$PERFORMANCE_FIXTURE_MODE" == slow ]]; then execute=5000; fi
for ((index=0; index<16; index++)); do
  echo "[harn test diag] ok case_${index} setup=1ms compile=0ms admission=0ms execute=${execute}ms teardown=0ms module_compile=14ms module_load=21ms modules_compiled=5 modules_loaded=5 total=5001ms" >&2
done
SH
chmod +x "$scratch/measured-harn"

for mode in slow child-failure success; do
  workspace="$scratch/$mode"
  mkdir -p "$workspace/scripts" "$workspace/bench/test-case-performance"
  cp "$repo_root/scripts/check_test_case_performance.harn" "$workspace/scripts/"
  cp "$repo_root/bench/test-case-performance/baselines.toml" "$workspace/bench/test-case-performance/"
  cp "$scratch/measured-harn" "$workspace/measured-harn"
  status=0
  (
    cd "$workspace"
    PERFORMANCE_FIXTURE_MODE="$mode" HARN_CHECK_BIN="$workspace/measured-harn" \
      "$HARN_BIN" run scripts/check_test_case_performance.harn
  ) >"$workspace/controller.stdout" 2>"$workspace/controller.stderr" || status=$?
  node --input-type=module - "$workspace" "$mode" "$status" "$workspace/measured-harn" <<'JS'
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
const [workspace, mode, status, executable] = process.argv.slice(2);
const parent = join(workspace, '.harn-runs/test-case-performance');
const directories = existsSync(parent) ? readdirSync(parent) : [];
if (mode === 'success') {
  assert.equal(Number(status), 0, readFileSync(join(workspace, 'controller.stderr'), 'utf8'));
  assert.equal(directories.length, 0, 'successful checks must clean their own evidence');
} else {
  assert.equal(Number(status), 1, 'the original failure must remain a failure');
  assert.equal(directories.length, 1, 'failed speed/child checks must retain evidence');
  const root = join(parent, directories[0]);
  const json = path => JSON.parse(readFileSync(join(root, path), 'utf8'));
  const identity = json('identity.json');
  assert.deepEqual(identity.executable.argv, [executable]);
  assert.equal(identity.executable.version.ok, true, JSON.stringify(identity.executable.version));
  assert.equal(JSON.parse(identity.executable.version.stdout).data.source_revision, 'fixture-source');
  assert.equal(identity.executable.digest.ok, true);
  const expectedDigest = createHash('sha256').update(readFileSync(executable)).digest('hex');
  assert.equal(identity.executable.digest.stdout.split(/\s+/)[0], expectedDigest);
  assert.equal(identity.script_sha256, createHash('sha256').update(readFileSync(join(workspace, 'scripts/check_test_case_performance.harn'))).digest('hex'));
  assert.equal(identity.resources.ok, true);
  assert.ok(identity.resources.stdout.length > 0);
  const verdict = json('verdict.json');
  assert.equal(verdict.verdict.ok, false);
  assert.equal(verdict.rounds, mode === 'slow' ? 3 : 1);
  assert.ok(verdict.verdict.failures.length > 0);
  for (let round = 1; round <= verdict.rounds; round++) {
    const measurement = json(`round-${round}/measurement.json`);
    assert.equal(measurement.runs.length, 4);
    assert.equal(measurement.metrics.test_count, mode === 'slow' ? 64 : 0);
    assert.equal(measurement.sample_errors.length === 0, mode === 'slow');
    for (const run of measurement.runs) {
      assert.equal(run.complete, true);
      assert.equal(run.status, mode === 'slow' ? 0 : 23);
      assert.ok(readFileSync(join(root, `round-${round}/run-${run.index}/stderr.txt`), 'utf8').length > 0);
      if (mode === 'slow') {
        assert.equal(run.diagnose.samples.length, 16);
        assert.equal(run.diagnose.samples[0].module_compile_ms, 14);
        assert.equal(run.diagnose.samples[0].module_load_ms, 21);
        assert.equal(run.diagnose.samples[0].admission_ms, 0);
        assert.equal(run.diagnose.samples[0].execute_ms, 5000);
      }
    }
  }
}
JS
done
echo 'test_case_performance_evidence_test: ok (slow, child failure, success)'
