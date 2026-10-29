import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const entry = fileURLToPath(new URL('../ci/release-reconcile-wake.mjs', import.meta.url));
const repository = 'burin-labs/harn';
const producer = {
  id: 40769657396,
  run_attempt: 1,
  head_sha: '3d26818716ecbb0ad760178968e8827d6b5825b3',
  head_branch: 'main',
  event: 'workflow_dispatch',
  status: 'completed',
  conclusion: 'failure',
  path: '.github/workflows/build-release-binaries.yml',
  repository: { full_name: repository },
  head_repository: { full_name: repository },
};

function runWake(run, options = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'release-reconcile-wake-'));
  try {
    const eventPath = join(dir, 'event.json');
    const outputPath = join(dir, 'output');
    writeFileSync(eventPath, JSON.stringify({ repository: { full_name: repository }, workflow_run: run, ...options.event }));
    writeFileSync(outputPath, '');
    const result = spawnSync(process.execPath, [entry], {
      encoding: 'utf8',
      env: {
        ...process.env,
        GITHUB_EVENT_PATH: eventPath,
        GITHUB_OUTPUT: outputPath,
        GITHUB_EVENT_NAME: options.eventName ?? 'workflow_run',
        GITHUB_REPOSITORY: repository,
        PLAN_RESULT: options.planResult ?? 'skipped',
        PLAN_RECONCILE: options.planReconcile ?? '',
      },
    });
    assert.equal(result.status, 0, result.stderr);
    return readFileSync(outputPath, 'utf8');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('failed trusted main producer wakes candidate reconciliation', () => {
  for (const conclusion of ['failure', 'cancelled', 'timed_out', 'startup_failure']) {
    assert.equal(runWake({ ...producer, conclusion }), 'required=true\n');
  }
});

test('successful plan preserves its existing reconciliation path', () => {
  assert.equal(runWake({ ...producer, conclusion: 'success' }, {
    planResult: 'success', planReconcile: 'true',
  }), 'required=true\n');
  assert.equal(runWake({ ...producer, conclusion: 'success' }, {
    planResult: 'success', planReconcile: 'false',
  }), 'required=false\n');
});

test('untrusted, incomplete or unrelated runs never wake reconciliation', () => {
  const rejected = [
    { ...producer, repository: { full_name: 'other/harn' } },
    { ...producer, head_repository: { full_name: 'other/harn' } },
    { ...producer, head_branch: 'feature' },
    { ...producer, event: 'pull_request' },
    { ...producer, status: 'in_progress' },
    { ...producer, conclusion: 'skipped' },
    { ...producer, conclusion: 'success' },
    { ...producer, path: '.github/workflows/other.yml' },
    { ...producer, id: 0 },
    { ...producer, run_attempt: undefined },
    { ...producer, head_sha: '0'.repeat(40) },
    { ...producer, head_sha: 'bad' },
  ];
  for (const run of rejected) assert.equal(runWake(run), 'required=false\n', JSON.stringify(run));
  assert.equal(runWake(producer, { eventName: 'workflow_dispatch' }), 'required=false\n');
  assert.equal(runWake(producer, { event: { repository: { full_name: 'other/harn' } } }), 'required=false\n');
  assert.equal(runWake(producer, { planResult: 'failure', planReconcile: 'true', eventName: 'workflow_dispatch' }), 'required=false\n');
});
