import assert from 'node:assert/strict';
import {test} from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {admitChild, observerDeclarationName, observerRunName, observerDispatchActor}
  from '../lib/consumer-observer.mjs';

test('the actual budget entrypoint authenticates archives and exact job timing before extending observation', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-budget-entry-'));
  try {
    const origin = 1792362069;
    const iso = seconds => new Date((origin + seconds) * 1000).toISOString().replace('.000Z', 'Z');
    const context = {schema: 'harn.consumer_observer.v2', parent_repository: 'burin-labs/harn',
      parent_run_id: '42', parent_run_attempt: 2, parent_head_sha: 'a'.repeat(40),
      source_revision: 'b'.repeat(40), dispatch_nonce: '01234567-89ab-4cde-8fab-0123456789ab',
      started_at: origin};
    const parent = {id: 42, run_attempt: 2, head_sha: context.parent_head_sha,
      head_repository: {full_name: context.parent_repository}, created_at: iso(-10),
      path: '.github/workflows/consumer-canary.yml', status: 'in_progress'};
    const child = {id: 73, run_attempt: 1, head_sha: 'c'.repeat(40),
      head_repository: {full_name: 'acme/widget-host'}, path: '.github/workflows/rehearsal.yml',
      event: 'workflow_dispatch', status: 'in_progress', display_title: observerRunName(context),
      actor: {login: observerDispatchActor, type: 'Bot'}};
    const expected = {repository: 'acme/widget-host', workflow: child.path, runId: '73',
      attempt: 1, headSha: child.head_sha, sourceRevision: context.source_revision};
    const ownership = admitChild(context, parent, expected, child,
      {inputs: {observer: JSON.stringify(context), source_revision: context.source_revision}}, context);
    const archives = [[observerDeclarationName(context), 'consumer-observer.json', context, parent],
      [`consumer-observer-${context.dispatch_nonce}`, `consumer-observer-${context.dispatch_nonce}.json`, ownership, child]]
      .map(([name, filename, value, run], index) => {
        const file = path.join(scratch, filename), archive = path.join(scratch, `${index}.zip`);
        fs.writeFileSync(file, JSON.stringify(value));
        const zip = spawnSync('zip', ['-j', archive, file], {encoding: 'utf8'});
        assert.equal(zip.status, 0, zip.stderr);
        const bytes = fs.readFileSync(archive);
        return {archive, metadata: {id: 501 + index, name, expired: false,
          size_in_bytes: bytes.length, digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
          workflow_run: {id: run.id, head_sha: run.head_sha}}};
      });
    const job = {id: 47, run_id: 73, run_attempt: 1, head_sha: child.head_sha,
      created_at: iso(74), started_at: iso(1842), completed_at: null,
      status: 'in_progress', conclusion: null, runner_id: 15, runner_name: 'owned-worker',
      steps: [{status: 'in_progress', conclusion: null, started_at: iso(1848)}]};
    const fixture = path.join(scratch, 'fixture.json'), preload = path.join(scratch, 'fetch.mjs');
    fs.writeFileSync(preload, `import fs from 'node:fs';
      const f=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
      let parentReads=0;
      Date.now=()=>f.now*1000;
      globalThis.fetch=async(url,options={})=>{
        if(options.method&&options.method!=='GET')throw new Error('unexpected-mutation');
        const e=new URL(url),p=e.pathname;
        if(p.endsWith('/42/attempts/2'))return {ok:true,json:async()=>++parentReads>1&&f.finalParent?f.finalParent:f.parent};
        if(p.endsWith('/73'))return {ok:true,json:async()=>f.finalChild??f.child};
        if(p.endsWith('/runs'))return {ok:true,json:async()=>({total_count:1,workflow_runs:[f.child]})};
        if(p.endsWith('/jobs'))return {ok:true,json:async()=>({total_count:f.partial?2:1,jobs:[f.job]})};
        if(p.endsWith('/artifacts')){
          const index=p.includes('/42/')?0:1;
          const items=f.missing===index?[]:[f.archives[index].metadata];
          return {ok:true,json:async()=>({total_count:items.length,artifacts:items})};
        }
        if(p.endsWith('/zip'))return {ok:true,arrayBuffer:async()=>fs.readFileSync(f.archives[p.includes('/501/')?0:1].archive)};
        throw new Error('unknown-api');
      };`);
    const execute = (changes = {}, env = {}) => {
      fs.writeFileSync(fixture, JSON.stringify({parent, child, archives, job, now: origin + 7200, ...changes}));
      return spawnSync(process.execPath, [fileURLToPath(new URL('../ci/consumer-observer-budget.mjs', import.meta.url))],
        {encoding: 'utf8', env: {PATH: process.env.PATH, NODE_OPTIONS: `--import=${preload}`,
          GH_TOKEN: 'fixture-read', GITHUB_REPOSITORY: context.parent_repository,
          GITHUB_RUN_ID: '42', GITHUB_RUN_ATTEMPT: '2', GITHUB_SHA: parent.head_sha,
          OBSERVER_CONTEXT: JSON.stringify(context), CANARY_REPOSITORY: expected.repository,
          CANARY_WORKFLOW: 'rehearsal.yml', CANARY_RUN_ID: '73', ...env}});
    };
    const positive = execute();
    assert.equal(positive.status, 0, positive.stderr);
    const read = JSON.parse(positive.stdout);
    assert.equal(read.owned, 1);
    assert.equal(read.observed_jobs, 1);
    assert.equal(read.reason, null);
    assert.equal(read.queue_seconds, 1768);
    assert.equal(read.execution_seconds, 5358);
    assert.doesNotMatch(positive.stdout + positive.stderr, /widget-host|owned-worker|head_sha|run_id/);
    const cancelledJob = {...job, status: 'completed', conclusion: 'cancelled',
      runner_id: 0, runner_name: '', steps: [], started_at: job.created_at, completed_at: iso(2714)};
    const cancelled = execute({child: {...child, status: 'completed', conclusion: 'cancelled'},
      job: cancelledJob, now: origin + 2775});
    assert.equal(cancelled.status, 0, cancelled.stderr);
    assert.deepEqual(JSON.parse(cancelled.stdout), {discovered: 1, owned: 1, reason: null,
      observed_jobs: 1, queued_jobs: 0, executing_jobs: 0, queue_seconds: 2640,
      execution_seconds: 0, wall_seconds: 2775});
    for (const completed_at of [null, 'bad-date', iso(73), iso(2776)]) {
      const unknownEnd = execute({job: {...cancelledJob, completed_at}, now: origin + 2775});
      assert.equal(unknownEnd.status, 1);
      assert.equal(unknownEnd.stdout, '');
      assert.match(unknownEnd.stderr, /reason=job_timing_unmeasured/);
    }
    for (const changes of [{partial: true}, {missing: 0},
      {job: {...job, run_attempt: 2}}, {job: {...job, head_sha: 'd'.repeat(40)}},
      {job: {...job, created_at: null}}, {job: {...job, created_at: 'bad-date'}},
      {finalChild: {...child, run_attempt: 2}}, {finalParent: {...parent, status: 'completed', conclusion: 'cancelled'}}]) {
      const negative = execute(changes);
      assert.equal(negative.status, 1, negative.stdout);
      assert.equal(negative.stdout, '');
      assert.doesNotMatch(negative.stderr, /widget-host|owned-worker|head_sha|run_id/);
    }
    const missing = execute({missing: 1});
    assert.equal(missing.status, 0, missing.stderr);
    const unknown = JSON.parse(missing.stdout);
    assert.equal(unknown.owned, 0);
    assert.equal(unknown.observed_jobs, null);
    assert.equal(unknown.reason, 'queue_deadline');
  } finally {
    fs.rmSync(scratch, {recursive: true});
  }
});
