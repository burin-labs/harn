import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { observerRunName, observerDeclarationName, observerDispatchActor, observerDeclarationStep } from '../lib/consumer-observer.mjs';

test('the inert declaration action projects the owning publication step identity', () => {
  const action = fs.readFileSync(new URL('../../.github/actions/consumer-observer-declare/action.yml', import.meta.url), 'utf8');
  assert.equal(/^name: (.+)$/m.exec(action)?.[1], observerDeclarationStep);
  const listener = fs.readFileSync(new URL('../../.github/workflows/consumer-observer-cleanup.yml', import.meta.url), 'utf8');
  assert.match(listener, /if: github\.event\.workflow_run\.head_repository\.full_name == github\.repository && github\.event\.workflow_run\.status == 'completed'/);
  assert.doesNotMatch(listener, /ref:.*workflow_run|download-artifact|checkout.*workflow_run/);
});

test('declaration preparation reaches a closed public file only for a measured live parent', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-publish-'));
  try {
    const output = path.join(scratch, 'output');
    const record = path.join(scratch, 'parent.json');
    const preload = path.join(scratch, 'fetch.mjs');
    const parent = { id: 42, run_attempt: 2, head_sha: 'a'.repeat(40),
      head_repository: { full_name: 'burin-labs/harn' }, path: '.github/workflows/consumer-canary.yml', status: 'in_progress' };
    fs.writeFileSync(preload, `import fs from 'node:fs';
      let clock=1000;
      Date.now=()=>clock*1000;
      globalThis.fetch = async () => {clock=1100;return {ok:true,json:async()=>JSON.parse(fs.readFileSync(${JSON.stringify(record)}, 'utf8'))};};`);
    const env = { PATH: process.env.PATH, NODE_OPTIONS: `--import=${preload}`, GH_TOKEN: 'fixture-read',
      GITHUB_REPOSITORY: 'burin-labs/harn', GITHUB_RUN_ID: '42', GITHUB_RUN_ATTEMPT: '2',
      GITHUB_SHA: parent.head_sha, SOURCE_REVISION: 'b'.repeat(40), RUNNER_TEMP: scratch, GITHUB_OUTPUT: output };
    const entry = fileURLToPath(new URL('../ci/consumer-observer-declare.mjs', import.meta.url));
    fs.writeFileSync(record, JSON.stringify(parent));
    const positive = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(positive.status, 0, positive.stderr);
    const fields = Object.fromEntries(fs.readFileSync(output, 'utf8').trim().split('\n').map(line => {
      const separator = line.indexOf('=');
      return [line.slice(0, separator), line.slice(separator + 1)];
    }));
    const declared = JSON.parse(fields.context);
    assert.deepEqual(JSON.parse(fs.readFileSync(fields.file, 'utf8')), declared);
    assert.equal(fields.name, observerDeclarationName(declared));
    assert.equal(declared.parent_run_id, '42');
    assert.equal(declared.started_at,1000,'the original issuer starts before declaration qualification and upload');
    assert.equal(Object.keys(declared).length, 8, 'public custody carries no private consumer identity');
    for (const reading of [{ ...parent, status: 'completed' }, { ...parent, run_attempt: 3 }, {}]) {
      fs.writeFileSync(record, JSON.stringify(reading));
      fs.writeFileSync(output, '');
      const negative = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
      assert.equal(negative.status, 1);
      assert.equal(fs.readFileSync(output, 'utf8'), '');
    }
  } finally {
    fs.rmSync(scratch, { recursive: true });
  }
});

test('the completion entrypoint reaches owned cancellation without publishing private identities', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-cleanup-'));
  try {
    const context = {
      schema: 'harn.consumer_observer.v2', parent_repository: 'burin-labs/harn',
      parent_run_id: '42', parent_run_attempt: 2, parent_head_sha: 'a'.repeat(40),
      source_revision: 'b'.repeat(40), dispatch_nonce: '01234567-89ab-4cde-8fab-0123456789ab', started_at: 1000,
    };
    const parent = { id: 42, run_attempt: 2, head_sha: context.parent_head_sha,
      head_repository: { full_name: context.parent_repository }, path: '.github/workflows/consumer-canary.yml',
      status: 'completed', conclusion: 'cancelled', created_at: '2026-10-11T20:00:00Z' };
    const child = { id: 73, run_attempt: 1, head_sha: 'c'.repeat(40),
      head_repository: { full_name: 'acme/widget-host' }, path: '.github/workflows/rehearsal.yml',
      status: 'in_progress', event: 'workflow_dispatch', actor: { login: observerDispatchActor, type: 'Bot' },
      display_title: observerRunName(context) };
    const receipt = { schema: 'harn.consumer_observer_receipt.v2', observer: context, child: {
      repository: 'acme/widget-host', workflow: child.path, run_id: '73', run_attempt: 1,
      head_sha: child.head_sha, dispatch_actor: observerDispatchActor,
      receipt_nonce: '11234567-89ab-4cde-8fab-0123456789ab',
    } };
    const archives = [];
    for (const [index, filename, value] of [[0, 'consumer-observer.json', context],
      [1, `consumer-observer-${context.dispatch_nonce}.json`, receipt]]) {
      const json = path.join(scratch, filename);
      const archive = path.join(scratch, `archive-${index}.zip`);
      fs.writeFileSync(json, JSON.stringify(value));
      const zipped = spawnSync('zip', ['-j', archive, json], { env: { PATH: process.env.PATH, LC_ALL: 'C' }, encoding: 'utf8' });
      assert.equal(zipped.status, 0, zipped.stderr);
      const bytes = fs.readFileSync(archive);
      archives.push({ archive, metadata: { id: 501 + index,
        name: index === 0 ? observerDeclarationName(context) : `consumer-observer-${context.dispatch_nonce}`,
        expired: false, size_in_bytes: bytes.length, digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
        workflow_run: { id: index === 0 ? 42 : 73, head_sha: index === 0 ? parent.head_sha : child.head_sha } } });
    }
    const config = path.join(scratch, 'fixture.json');
    const eventFile = path.join(scratch, 'event.json');
    const callsFile = path.join(scratch, 'calls.json');
    const preload = path.join(scratch, 'fetch.mjs');
    fs.writeFileSync(eventFile, JSON.stringify({ action: 'completed', repository: { full_name: context.parent_repository }, workflow_run: parent }));
    fs.writeFileSync(preload, `import fs from 'node:fs';
      const fixture = JSON.parse(fs.readFileSync(${JSON.stringify(config)}, 'utf8'));
      const calls = [];
      let cancelled = false;
      globalThis.fetch = async (url, options = {}) => {
        const p = new URL(url).pathname;
        calls.push({method: options.method ?? 'GET', path: p});
        fs.writeFileSync(${JSON.stringify(callsFile)}, JSON.stringify(calls));
        if (p.endsWith('/cancel')) {cancelled = true; return {status:202};}
        if (p.endsWith('/zip')) {
          const a = fixture.archives[p.includes('/501/') ? 0 : 1];
          return {ok:true,arrayBuffer:async()=>fs.readFileSync(a.archive)};
        }
        let record;
        if (p.endsWith('/42/attempts/2')) record=fixture.parent;
        else if (p.endsWith('/jobs')) record={total_count:1,jobs:[{id:801,name:fixture.jobName,status:'completed',conclusion:'success',steps:fixture.steps}]};
        else if (p.endsWith('/artifacts')) {
          const index=p.includes('/42/') ? 0 : 1;
          const missing=fixture.missing === index;
          record={total_count:missing ? 0 : 1,artifacts:missing ? [] : [fixture.archives[index].metadata]};
        }
        else if (p.endsWith('/rehearsal.yml/runs')) record={total_count:2,workflow_runs:[fixture.child,{...fixture.child,id:74,display_title:'ordinary reused success',status:'completed',conclusion:'success'}]};
        else if (p.endsWith('/73')) record=cancelled ? {...fixture.child,status:'completed',conclusion:'cancelled'} : fixture.child;
        else throw new Error('fixture-private-url-unknown');
        return {ok:true,json:async()=>record};
      };`);
    const env = { PATH: process.env.PATH, NODE_OPTIONS: `--import=${preload}`,
      GITHUB_EVENT_PATH: eventFile, GITHUB_EVENT_NAME: 'workflow_run', GITHUB_REPOSITORY: context.parent_repository,
      GH_TOKEN: 'fixture-read', CANARY_OWNER: 'acme', CANARY_NAME: 'widget-host', CANARY_WORKFLOW: 'rehearsal.yml' };
    const entry = fileURLToPath(new URL('../ci/consumer-observer-reconcile.mjs', import.meta.url));
    const execute = changes => {
      fs.writeFileSync(config, JSON.stringify({ parent, child, archives, jobName: 'Declare consumer observer',
        steps: [{ name: observerDeclarationStep, status: 'completed', conclusion: 'success' }], ...changes }));
      const run = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
      assert.doesNotMatch(run.stdout + run.stderr, /widget-host|\/73|\/74|run_id|fixture-private/);
      return { run, posts: JSON.parse(fs.readFileSync(callsFile, 'utf8')).filter(call => call.method === 'POST') };
    };
    const positive = execute({});
    assert.equal(positive.run.status, 0, positive.run.stderr);
    assert.match(positive.run.stdout, /discovered=1 owned=1 terminal=1 cancelled=1 pending=0 refused=0/);
    assert.deepEqual(positive.posts.map(call => call.path), ['/repos/acme/widget-host/actions/runs/73/cancel']);
    for (const changes of [{ missing: 0 }, { missing: 1 }, { child: { ...child, run_attempt: 2 } },
      { child: { ...child, actor: { login: 'unrelated[bot]', type: 'Bot' } } },
      { parent: { ...parent, head_repository: { full_name: 'unrelated/foreign-parent' } } }]) {
      const negative = execute(changes);
      assert.equal(negative.run.status, 1);
      assert.equal(negative.posts.length, 0);
    }
    for (const [workflow, jobName] of [
      ['consumer-canary.yml', 'Declare consumer observer'],
      ['build-release-binaries.yml', 'Consumer release rehearsal / Declare consumer observer'],
      ['promote-release.yml', 'Recover missing consumer rehearsal / Declare consumer observer'],
    ]) {
      const negative = execute({ missing: 0, jobName, parent: { ...parent, path: `.github/workflows/${workflow}` } });
      assert.equal(negative.run.status, 1);
      assert.match(negative.run.stderr, /reason=successful_declaration_missing/);
      assert.equal(negative.posts.length, 0, 'caller prefixes cannot turn missing declaration custody into ordinary zero');
    }
    for (const steps of [null, [], [{ name: observerDeclarationStep, status: 'unknown', conclusion: null }]]) {
      const partialSteps = execute({ missing: 0, steps });
      assert.equal(partialSteps.run.status, 1);
      assert.match(partialSteps.run.stderr, /reason=declaration_step_census_unmeasured/);
      assert.equal(partialSteps.posts.length, 0);
    }
    const partialPublication = execute({ steps: [
      { name: observerDeclarationStep, status: 'completed', conclusion: 'success' },
      { name: observerDeclarationStep, status: 'completed', conclusion: 'success' },
    ] });
    assert.equal(partialPublication.run.status, 1);
    assert.match(partialPublication.run.stderr, /reason=declaration_publication_ambiguous/);
    assert.equal(partialPublication.posts.length, 0);
    const failedPublication = execute({ steps: [
      { name: observerDeclarationStep, status: 'completed', conclusion: 'failure' },
    ] });
    assert.equal(failedPublication.run.status, 1);
    assert.match(failedPublication.run.stderr, /reason=declaration_publication_unmeasured/);
    assert.equal(failedPublication.posts.length, 0);
  } finally {
    fs.rmSync(scratch, { recursive: true });
  }
});
