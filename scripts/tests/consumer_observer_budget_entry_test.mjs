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
        const role=p.startsWith('/repos/burin-labs/harn/')?'fixture-parent':'fixture-read';
        if(options.headers?.Authorization!=='Bearer '+role)throw new Error('incorrect-token-role');
        fs.appendFileSync(f.calls,JSON.stringify({path:p,parent:role==='fixture-parent'})+String.fromCharCode(10));
        if(f.parentDenied&&role==='fixture-parent')return {ok:false,status:403,json:async()=>({message:'Forbidden'})};
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
      const calls=path.join(scratch,'calls.jsonl');
      fs.writeFileSync(calls,'');
      fs.writeFileSync(fixture, JSON.stringify({parent, child, archives, job, calls, now: origin + 7200, ...changes}));
      const result=spawnSync(process.execPath, [fileURLToPath(new URL('../ci/consumer-observer-budget.mjs', import.meta.url))],
        {encoding: 'utf8', env: {PATH: process.env.PATH, NODE_OPTIONS: `--import=${preload}`,
          GH_TOKEN: 'fixture-read', GITHUB_TOKEN: 'fixture-parent', GITHUB_REPOSITORY: context.parent_repository,
          GITHUB_RUN_ID: '42', GITHUB_RUN_ATTEMPT: '2', GITHUB_SHA: parent.head_sha,
          OBSERVER_CONTEXT: JSON.stringify(context), CANARY_REPOSITORY: expected.repository,
          CANARY_WORKFLOW: 'rehearsal.yml', CANARY_RUN_ID: '73', ...env}});
      result.reads=fs.readFileSync(calls,'utf8').trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
      return result;
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
      execution_seconds: 0, wall_seconds: 2775, child_status:'completed', child_conclusion:'cancelled',
      custody_read:'fresh',read_attempts:{repository:4,app:6}});
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
    for (const env of [{GITHUB_TOKEN:''},{GH_TOKEN:''}]) {
      const refused=execute({},env);
      assert.equal(refused.status,1);
      assert.equal(refused.reads.length,0,'missing roles must refuse before any read');
    }
    const denied=execute({parentDenied:true});
    assert.equal(denied.status,1);
    assert.equal(denied.reads.length,1);
    assert(denied.reads.every(read=>read.parent),'a refused local read never falls back to App authority');

    const cache=path.join(scratch,'checkpoint.json.timing');
    const cacheEnv={CANARY_CHECKPOINT_FILE:path.join(scratch,'checkpoint.json'),CANARY_TIMING_CACHE_KEY:'1'.repeat(64)};
    const first=execute({},cacheEnv);
    assert.equal(first.status,0,first.stderr);
    assert.equal(JSON.parse(first.stdout).owned,1);
    assert.equal(first.reads.filter(read=>!read.parent).length,6);
    const qualified=JSON.parse(fs.readFileSync(cache,'utf8'));
    assert.equal(fs.statSync(cache).mode&0o777,0o600);
    assert(!fs.readFileSync(cache,'utf8').includes(cacheEnv.CANARY_TIMING_CACHE_KEY));
    for(let poll=0;poll<12;poll++){
      const next=execute({},cacheEnv);
      assert.equal(next.status,0,next.stderr);
      assert.equal(JSON.parse(next.stdout).observed_jobs,1);
      assert.equal(JSON.parse(next.stdout).custody_read,'retained');
      assert.deepEqual(JSON.parse(next.stdout).read_attempts,{repository:2,app:2});
      assert.equal(next.reads.filter(read=>!read.parent).length,2,'pending polls read the live complete jobs and child');
      assert.equal(next.reads.filter(read=>read.parent).length,2,'both parent observations use repository authority');
      assert(!next.reads.some(read=>/artifacts|\/runs$/.test(read.path)));
    }
    const staleJobs=execute({job:{...job,run_attempt:2}},cacheEnv);
    assert.equal(staleJobs.status,1,'retained custody cannot turn stale job evidence into a verdict');
    assert.equal(staleJobs.stdout,'');
    const rerun=execute({finalChild:{...child,run_attempt:2}},cacheEnv);
    assert.equal(rerun.status,1,'a fresh rerun cannot inherit the prior attempt receipt');
    assert.equal(rerun.stdout,'');
    assert(rerun.reads.some(read=>read.path.endsWith('/zip')),'a changed attempt reauthenticates original custody');
    assert(!fs.existsSync(cache),'the prior attempt cache is retired before fresh qualification');
    assert.equal(execute({},cacheEnv).status,0);
    const terminal=execute({child:{...child,status:'completed',conclusion:'success'},
      job:{...job,status:'completed',conclusion:'success',completed_at:iso(7210)},now:origin+7220},cacheEnv);
    assert.equal(terminal.status,0,terminal.stderr);
    assert(terminal.reads.some(read=>read.path.endsWith('/zip')),'terminal timing reauthenticates the actual archives');
    assert(!fs.existsSync(cache),'terminal refresh retires pending proof');

    fs.writeFileSync(cache,JSON.stringify({...qualified,mac:'0'.repeat(64)}),{mode:0o600});
    const forged=execute({missing:0},cacheEnv);
    assert.equal(forged.status,1,'a valid-shaped forged preseed must still require the original declaration');
    assert.equal(forged.stdout,'');
    assert(forged.reads.some(read=>read.parent&&read.path.endsWith('/artifacts')));
    fs.writeFileSync(cache,JSON.stringify(qualified));
    const renewed=execute({}, {...cacheEnv,CANARY_TIMING_CACHE_KEY:'2'.repeat(64)});
    assert.equal(renewed.status,0,renewed.stderr);
    assert.equal(renewed.reads.filter(read=>!read.parent).length,6,'a new invocation reauthenticates before retaining proof');
    fs.chmodSync(cache,0o666);
    const writable=execute({},cacheEnv);
    assert.equal(writable.status,1);
    assert.equal(writable.stdout,'');
    assert.equal(writable.reads.filter(read=>!read.parent).length,0);
  } finally {
    fs.rmSync(scratch, {recursive: true});
  }
});
