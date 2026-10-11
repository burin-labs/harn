import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import {
  admitChild, admitObserver, githubRecord, observerContext, observerRunName,
  observerDeclarationName, observerRunNameExpression, observerDispatchActor,
} from '../lib/consumer-observer.mjs';

const context = {
  schema: 'harn.consumer_observer.v1',
  parent_repository: 'burin-labs/harn',
  parent_run_id: '42',
  parent_run_attempt: 2,
  parent_head_sha: 'a'.repeat(40),
  source_revision: 'b'.repeat(40),
  dispatch_nonce: '01234567-89ab-4cde-8fab-0123456789ab',
};
const parent = {
  id: 42, run_attempt: 2, head_sha: context.parent_head_sha,
  head_repository: { full_name: context.parent_repository },
  path: '.github/workflows/build-release-binaries.yml', status: 'in_progress',
};
const expected = {
  repository: 'acme/widget-host', runId: '73', attempt: 1,
  headSha: 'c'.repeat(40), workflow: '.github/workflows/rehearsal.yml',
  sourceRevision: context.source_revision,
};
const child = {
  id: 73, run_attempt: 1, head_sha: expected.headSha,
  head_repository: { full_name: expected.repository },
  path: expected.workflow, event: 'workflow_dispatch', status: 'in_progress',
  actor: { login: observerDispatchActor, type: 'Bot' },
  display_title: observerRunName(context),
};
const event = { inputs: { observer: JSON.stringify(context), source_revision: context.source_revision } };

test('admission binds an authenticated child to a live current parent and separately frozen source', () => {
  const receipt = admitChild(context, parent, expected, child, event, context);
  assert.equal(receipt.child.run_id, '73');
  assert.deepEqual(receipt.observer, observerContext(context));
  assert.notEqual(receipt.observer.parent_head_sha, receipt.observer.source_revision);
});

test('dead, superseded, malformed, and unrelated parent readings cannot admit work', () => {
  for (const record of [
    null, {}, { ...parent, id: 41 }, { ...parent, head_sha: 'd'.repeat(40) },
    { ...parent, run_attempt: 3 }, { ...parent, status: 'completed' },
    { ...parent, status: undefined }, { ...parent, path: '.github/workflows/unrelated.yml' },
    { ...parent, head_repository: { full_name: 'acme/fork' } },
  ]) assert.throws(() => admitObserver(context, record), /CONSUMER_OBSERVER reason=/);
  assert.throws(() => observerContext({ ...context, extra: true }), /context_shape_invalid/);
  assert.throws(() => observerContext({ ...context, parent_run_id: '' }), /context_identity_invalid/);
});

test('a matching run name cannot substitute for dispatch custody', () => {
  for (const record of [
    { ...child, id: 74 }, { ...child, path: '.github/workflows/other.yml' },
    { ...child, event: 'pull_request' }, { ...child, head_sha: 'd'.repeat(40) },
    { ...child, actor: { login: 'unrelated[bot]', type: 'Bot' } },
    { ...child, actor: { login: observerDispatchActor, type: 'User' } },
    { ...child, run_attempt: 2 },
  ]) assert.throws(() => admitChild(context, parent, expected, record, event, context), /child_dispatch_identity_unproven/);
  assert.throws(() => admitChild(context, parent, expected, child, {
    inputs: { ...event.inputs, source_revision: 'd'.repeat(40) },
  }, context), /child_dispatch_identity_unproven/);
  assert.throws(() => admitChild(context, parent, expected, child, {
    inputs: { ...event.inputs, observer: JSON.stringify({ ...context, parent_head_sha: 'd'.repeat(40) }) },
  }, context), /child_dispatch_identity_unproven/);
  for (const status of ['completed', 'unknown', undefined]) {
    assert.throws(() => admitChild(context, parent, expected, { ...child, status }, event, context), /child_state_unmeasured_or_ended/);
  }
});

test('workflow run-name projection agrees mechanically with the owning renderer', () => {
  const fields = [...observerRunNameExpression.matchAll(/fromJSON\(inputs.observer\)\.([a-z_]+)/g)]
    .map(match => match[1]);
  const template = /^format\('([^']+)'/.exec(observerRunNameExpression)[1];
  const projected = template.replace(/\{([0-9]+)\}/g, (_, index) => context[fields[Number(index)]]);
  assert.equal(projected, observerRunName(context));
  assert.ok(projected.length < 255);
  const documentation = fs.readFileSync(new URL('../../docs/src/dev/consumer-observer.md', import.meta.url), 'utf8');
  assert.ok(documentation.includes(observerRunNameExpression), 'documented caller projection must match the owner');
});

test('missing authenticated API records are unknown, while a nonempty reading reaches admission', async () => {
  const response = record => async () => ({ ok: true, json: async () => record });
  assert.deepEqual(await githubRecord('fixture-token', context.parent_repository, '42', response(parent)), parent);
  for (const record of [null, {}, { ...parent, id: 43 }, { ...parent, run_attempt: undefined }]) {
    await assert.rejects(githubRecord('fixture-token', context.parent_repository, '42', response(record)), /run_read_unmeasured/);
  }
  await assert.rejects(githubRecord('', context.parent_repository, '42', response(parent)), /read_authority_or_identity_missing/);
  await assert.rejects(githubRecord('fixture-token', context.parent_repository, '42', async () => ({ ok: false })), /run_read_refused/);
});

test('the action entrypoint reaches private receipt creation and refuses dead parents without publishing identities', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-entry-'));
  try {
    const output = path.join(scratch, 'output');
    const eventFile = path.join(scratch, 'event.json');
    const preload = path.join(scratch, 'fetch.mjs');
    const parentFile = path.join(scratch, 'parent.json');
    const childFile = path.join(scratch, 'child.json');
    const censusFile = path.join(scratch, 'census.json');
    const declarationFile = path.join(scratch, 'consumer-observer.json');
    const archiveFile = path.join(scratch, 'declaration.zip');
    fs.writeFileSync(declarationFile, JSON.stringify(context));
    const zipped = spawnSync('zip', ['-j', archiveFile, declarationFile], {
      env: { PATH: process.env.PATH, LC_ALL: 'C' }, encoding: 'utf8',
    });
    assert.equal(zipped.status, 0, zipped.stderr);
    const archiveBytes = fs.readFileSync(archiveFile);
    const census = { total_count: 1, artifacts: [{
      id: 11, name: observerDeclarationName(context), expired: false,
      workflow_run: { id: 42, head_sha: context.parent_head_sha },
      size_in_bytes: archiveBytes.length,
      digest: `sha256:${createHash('sha256').update(archiveBytes).digest('hex')}`,
    }] };
    fs.writeFileSync(censusFile, JSON.stringify(census));
    fs.writeFileSync(eventFile, JSON.stringify(event));
    fs.writeFileSync(preload, `import fs from 'node:fs';
      let parentReads = 0;
      function parentRead() {
        const record = JSON.parse(fs.readFileSync(${JSON.stringify(parentFile)}, 'utf8'));
        return record.fixture_end_after_first_read && ++parentReads > 1
          ? {...record, status: 'completed'} : record;
      }
      function artifactRead(url) {
        const name = new URL(url).searchParams.get('name');
        if (!name) {
          return {total_count: 101, artifacts: Array.from({length: 100}, (_, index) =>
            ({id: index + 100, name: 'unrelated-build-artifact-' + index}))};
        }
        if (name !== ${JSON.stringify(observerDeclarationName(context))}) {
          return {total_count: 0, artifacts: []};
        }
        return JSON.parse(fs.readFileSync(${JSON.stringify(censusFile)}, 'utf8'));
      }
      globalThis.fetch = async url => ({ok: true,
        arrayBuffer: async () => fs.readFileSync(${JSON.stringify(archiveFile)}),
        json: async () => url.includes('/artifacts?')
          ? artifactRead(url)
          : url.endsWith('/42') ? parentRead()
          : JSON.parse(fs.readFileSync(${JSON.stringify(childFile)}, 'utf8'))});`);
    const env = {
      PATH: process.env.PATH,
      NODE_OPTIONS: `--import=${preload}`,
      OBSERVER_CONTEXT: JSON.stringify(context),
      GH_TOKEN: 'fixture-token',
      GITHUB_OUTPUT: output,
      GITHUB_EVENT_PATH: eventFile,
      GITHUB_REPOSITORY: expected.repository,
      GITHUB_RUN_ID: expected.runId,
      GITHUB_RUN_ATTEMPT: String(expected.attempt),
      GITHUB_SHA: expected.headSha,
      GITHUB_WORKFLOW_REF: `${expected.repository}/${expected.workflow}@refs/heads/main`,
      SOURCE_REVISION: expected.sourceRevision,
      RUNNER_TEMP: scratch,
    };
    const entry = fileURLToPath(new URL('../ci/consumer-observer-admit.mjs', import.meta.url));
    fs.writeFileSync(parentFile, JSON.stringify(parent));
    fs.writeFileSync(childFile, JSON.stringify(child));
    const admitted = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(admitted.status, 0, admitted.stderr);
    assert.match(fs.readFileSync(output, 'utf8'), /^owned=true\n/);
    const receiptFile = path.join(scratch, `consumer-observer-${context.dispatch_nonce}.json`);
    assert.equal(JSON.parse(fs.readFileSync(receiptFile, 'utf8')).child.run_id, expected.runId);
    assert.equal(fs.statSync(receiptFile).mode & 0o777, 0o600);
    assert.doesNotMatch(admitted.stdout + admitted.stderr, /widget-host|run_id|73/);

    fs.rmSync(receiptFile);
    fs.writeFileSync(output, '');
    fs.writeFileSync(childFile, JSON.stringify({ ...child, actor: { login: 'unrelated[bot]', type: 'Bot' } }));
    const forgedActor = spawnSync(process.execPath, [entry], {
      env: { ...env, OBSERVER_DISPATCH_ACTOR: 'unrelated[bot]' }, encoding: 'utf8',
    });
    assert.equal(forgedActor.status, 1);
    assert.match(forgedActor.stderr, /reason=child_dispatch_identity_unproven/);
    assert.equal(fs.existsSync(receiptFile), false);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    fs.writeFileSync(childFile, JSON.stringify(child));
    fs.writeFileSync(censusFile, JSON.stringify({ total_count: 0, artifacts: [] }));
    const unissued = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(unissued.status, 1);
    assert.match(unissued.stderr, /reason=dispatch_declaration_missing_or_ambiguous/);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    assert.equal(fs.existsSync(receiptFile), false);
    fs.writeFileSync(censusFile, JSON.stringify(census));
    const forged = { ...context, dispatch_nonce: '11234567-89ab-4cde-8fab-0123456789ab' };
    const forgedAttempt = spawnSync(process.execPath, [entry], {
      env: { ...env, OBSERVER_CONTEXT: JSON.stringify(forged) }, encoding: 'utf8',
    });
    assert.equal(forgedAttempt.status, 1);
    assert.match(forgedAttempt.stderr, /reason=dispatch_declaration_missing_or_ambiguous/);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    for (const [reading, reason] of [
      [{ total_count: 2, artifacts: [census.artifacts[0], { ...census.artifacts[0], id: 12 }] }, 'missing_or_ambiguous'],
      [{ ...census, total_count: 2 }, 'census_unmeasured'],
      [{ ...census, artifacts: [{ ...census.artifacts[0], expired: true }] }, 'identity_unproven'],
      [{ ...census, artifacts: [{ ...census.artifacts[0], size_in_bytes: 0 }] }, 'identity_unproven'],
      [{ ...census, artifacts: [{ ...census.artifacts[0], size_in_bytes: -1 }] }, 'identity_unproven'],
      [{ ...census, artifacts: [{ ...census.artifacts[0], digest: `sha256:${'0'.repeat(64)}` }] }, 'digest_mismatch'],
    ]) {
      fs.writeFileSync(censusFile, JSON.stringify(reading));
      const refused = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
      assert.equal(refused.status, 1);
      assert.match(refused.stderr, new RegExp(`reason=dispatch_declaration_${reason}`));
      assert.equal(fs.readFileSync(output, 'utf8'), '');
      assert.equal(fs.existsSync(receiptFile), false);
    }
    fs.writeFileSync(censusFile, JSON.stringify(census));
    fs.writeFileSync(parentFile, JSON.stringify({ ...parent, status: 'completed' }));
    const abandoned = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(abandoned.status, 1);
    assert.match(abandoned.stderr, /reason=parent_ended/);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    assert.equal(fs.existsSync(receiptFile), false);

    fs.writeFileSync(parentFile, JSON.stringify({ ...parent, fixture_end_after_first_read: true }));
    const endedDuringRead = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(endedDuringRead.status, 1);
    assert.match(endedDuringRead.stderr, /reason=parent_ended/);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    assert.equal(fs.existsSync(receiptFile), false);

    const ordinary = spawnSync(process.execPath, [entry], {
      env: { PATH: process.env.PATH, GITHUB_OUTPUT: output }, encoding: 'utf8',
    });
    assert.equal(ordinary.status, 0, ordinary.stderr);
    assert.equal(fs.readFileSync(output, 'utf8'), 'owned=false\n');
  } finally {
    fs.rmSync(scratch, { recursive: true });
  }
});
