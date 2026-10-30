import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';
import { load } from 'js-yaml';
import { createObserverContext, observerRunName, observerDeclarationName, observerPublicationWitness, qualifyObserverPublisher, githubParentDeclarations, observerDispatchActor, observerDeclarationStep, observerDispatchStep } from '../lib/consumer-observer.mjs';

test('manual cleanup rejects feature refs and checks out trusted code before credentials', () => {
  const workflow = load(fs.readFileSync(new URL('../../.github/workflows/consumer-observer-cleanup.yml', import.meta.url), 'utf8'));
  const job = workflow.jobs.reconcile;
  const permitted = (event, ref, repository = 'burin-labs/harn') => runInNewContext(job.if, {
    github: { event_name: event, ref, repository: 'burin-labs/harn',
      event: { workflow_run: { head_repository: { full_name: repository },
        status: 'completed', name: 'Consumer canary', event: 'workflow_dispatch' } } },
  });
  assert.equal(permitted('workflow_dispatch', 'refs/heads/main'), true);
  assert.equal(permitted('workflow_dispatch', 'refs/heads/feature'), false);
  assert.equal(permitted('schedule', 'refs/heads/main'), true);
  assert.equal(permitted('workflow_run', 'refs/heads/main'), true);
  assert.equal(permitted('workflow_run', 'refs/heads/main', 'unrelated/repository'), false);
  const checkout = job.steps.findIndex(step => step.uses?.startsWith('actions/checkout@'));
  const token = job.steps.findIndex(step => step.uses?.startsWith('actions/create-github-app-token@'));
  assert.ok(checkout >= 0 && token > checkout);
  assert.equal(job.steps[checkout].with.ref, 'refs/heads/main');
  assert.equal(job.steps[checkout].with['persist-credentials'], false);
});

test('the inert declaration action projects the owning publication step identity', () => {
  const action = fs.readFileSync(new URL('../../.github/actions/consumer-observer-declare/action.yml', import.meta.url), 'utf8');
  assert.equal(/^name: (.+)$/m.exec(action)?.[1], observerDeclarationStep);
  const listener = fs.readFileSync(new URL('../../.github/workflows/consumer-observer-cleanup.yml', import.meta.url), 'utf8');
  assert.match(listener, /cron: '8,23,38,53 \* \* \* \*'/);
  assert.match(listener, /workflows: \[Consumer canary, Build release binaries, Promote release, CI\]/);
  assert.doesNotMatch(listener, /ref:.*workflow_run|download-artifact|checkout.*workflow_run/);
  const workflow = load(fs.readFileSync(new URL('../../.github/workflows/consumer-canary.yml', import.meta.url), 'utf8'));
  const steps = workflow.jobs.consumers.steps;
  const publication = steps.findIndex(step => step.id === 'observer');
  const witness = steps[publication + 1];
  const context = createObserverContext({runId:'42',attempt:1,headSha:'a'.repeat(40),sourceRevision:'b'.repeat(40)});
  const rendered = witness.name.replace("${{ fromJSON(steps.observer.outputs.context || '{}').dispatch_nonce }}",context.dispatch_nonce);
  assert.equal(rendered,observerPublicationWitness(context));
  assert.equal(witness.run, 'node scripts/ci/consumer-observer-declare.mjs --witness');
  assert.equal(witness.env.OBSERVER_CONTEXT, '${{ steps.observer.outputs.context }}');
  assert.equal(steps[publication + 2].name, observerDispatchStep);
});

test('candidate and baseline custody pairs each authenticated nonce to its own publisher', async () => {
  const context = {schema:'harn.consumer_observer.v2',parent_repository:'burin-labs/harn',
    parent_run_id:'41044671103',parent_run_attempt:1,parent_head_sha:'23b1a0fe1f2ca7a361bbeb0eef150149c09d48b1',
    source_revision:'43c8228630c66aecd1c74aab0144a2fc103bf9f8',
    dispatch_nonce:'be040d3a-74ae-472d-b8bc-1a373f6486fb',started_at:1793350000};
  const baseline = {...context,source_revision:'bd0ed627228d5433ebb2c6b7e81744d218d05d79',
    dispatch_nonce:'ac917952-af08-4207-8a87-dde1636c6d78'};
  const publisher = (owner,id) => ({id,run_id:Number(owner.parent_run_id),run_attempt:owner.parent_run_attempt,
    head_sha:owner.parent_head_sha,status:'completed',conclusion:'success',steps:[
      {name:observerDeclarationStep,number:7,status:'completed',conclusion:'success'},
      {name:observerPublicationWitness(owner),number:8,status:'completed',conclusion:'success'}]});
  const jobs = [publisher(context,124029936391),publisher(baseline,124043792303)];
  for (const [index,owner] of [context,baseline].entries()) {
    assert.equal(qualifyObserverPublisher(owner,jobs).job.id,jobs[index].id);
    const bad = jobs.map(job => ({...job,steps:job.steps.map(step=>({...step}))}));
    const target = bad[index];
    for (const change of [
      {steps:target.steps.slice(0,1)},
      {steps:[target.steps[0],{...target.steps[1],name:observerPublicationWitness(index===0?baseline:context)}]},
      {steps:[...target.steps,target.steps[1]]},
      {steps:[target.steps[0],{...target.steps[1],conclusion:'failure'}]},
      {steps:[target.steps[0],{...target.steps[1],number:6}]},
      {head_sha:'f'.repeat(40)},{run_attempt:2},{run_id:42},{id:null},
    ]) {
      assert.throws(()=>qualifyObserverPublisher(owner,bad.map((job,i)=>i===index?{...target,...change}:job)),
        /observer_job_identity_unproven/);
    }
    assert.throws(()=>qualifyObserverPublisher({...owner,dispatch_nonce:'11234567-89ab-4cde-8fab-0123456789ab'},jobs),
      /observer_job_identity_unproven/);
  }
  const parent = {id:Number(context.parent_run_id),run_attempt:1,head_sha:context.parent_head_sha,
    head_repository:{full_name:context.parent_repository},path:'.github/workflows/ci.yml',status:'completed',conclusion:'failure'};
  const values = [context,baseline].map(value=>Buffer.from(JSON.stringify(value)));
  const artifacts = values.map((bytes,index)=>({id:501+index,name:observerDeclarationName([context,baseline][index]),
    expired:false,size_in_bytes:bytes.length,digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,
    workflow_run:{id:parent.id,head_sha:parent.head_sha}}));
  let measuredJobs = jobs;
  const read = async url => {
    const p = new URL(url).pathname;
    if (p.endsWith('/jobs')) return {ok:true,json:async()=>({total_count:measuredJobs.length,jobs:measuredJobs})};
    if (p.endsWith('/artifacts')) return {ok:true,json:async()=>({total_count:artifacts.length,artifacts})};
    if (p.endsWith('/zip')) return {ok:true,arrayBuffer:async()=>values[Number(p.split('/').at(-2))-501]};
    return {ok:true,json:async()=>parent};
  };
  const declarations = () => githubParentDeclarations('fixture',context.parent_run_id,1,read,bytes=>JSON.parse(bytes));
  assert.deepEqual(await declarations(),[context,baseline]);
  measuredJobs = [jobs[0],{...jobs[1],steps:[jobs[1].steps[0]]}];
  await assert.rejects(declarations(),/observer_job_identity_unproven/);
  measuredJobs = [...jobs,{...jobs[0],id:991}];
  await assert.rejects(declarations(),/successful_declaration_missing/);

  const frozen = {...context,parent_run_id:'41016965350',parent_run_attempt:3,
    parent_head_sha:'e1ef4c6ae5bcf6445a3b864712f2589f3d8aabcc',source_revision:'e1ef4c6ae5bcf6445a3b864712f2589f3d8aabcc'};
  const legacy = publisher(frozen,99); legacy.steps.pop();
  assert.equal(qualifyObserverPublisher(frozen,[legacy]).job.id,99);
  for (const change of [{parent_run_attempt:4},{parent_run_id:'41016965351'},
    {parent_head_sha:'e'.repeat(40)},{source_revision:context.source_revision}]) {
    assert.throws(()=>qualifyObserverPublisher({...frozen,...change},[legacy]),/observer_job_identity_unproven/);
  }
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
    const archive = path.join(scratch,'declaration.zip');
    const zipped = spawnSync('zip',['-j',archive,fields.file],{encoding:'utf8'});
    assert.equal(zipped.status,0,zipped.stderr);
    const bytes = fs.readFileSync(archive);
    const artifact = {id:501,name:fields.name,expired:false,size_in_bytes:bytes.length,
      digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,
      workflow_run:{id:42,head_sha:parent.head_sha}};
    const witnessConfig = path.join(scratch,'witness.json');
    const witnessPreload = path.join(scratch,'witness-fetch.mjs');
    fs.writeFileSync(witnessPreload,`import fs from 'node:fs';
      const fixture=JSON.parse(fs.readFileSync(${JSON.stringify(witnessConfig)},'utf8'));
      globalThis.fetch=async url=>{
        const endpoint=new URL(url);
        if(endpoint.pathname.endsWith('/artifacts')){
          const selected=fixture.artifacts.filter(a=>a.name===endpoint.searchParams.get('name'));
          return {ok:true,json:async()=>({total_count:selected.length,artifacts:selected})};
        }
        if(endpoint.pathname.endsWith('/zip'))return {ok:true,arrayBuffer:async()=>fs.readFileSync(${JSON.stringify(archive)})};
        return {ok:true,json:async()=>fixture.parent};
      };`);
    const witness = (changes={},extraEnv={}) => {
      fs.writeFileSync(witnessConfig,JSON.stringify({parent,artifacts:[artifact],...changes}));
      return spawnSync(process.execPath,[entry,'--witness'],{encoding:'utf8',
        env:{...env,NODE_OPTIONS:`--import=${witnessPreload}`,OBSERVER_CONTEXT:fields.context,...extraEnv}});
    };
    const reached = witness();
    assert.equal(reached.status,0,reached.stderr);
    assert.match(reached.stdout,/publication=authenticated parent=live attempt=current/);
    for (const [changes,extraEnv] of [[{artifacts:[]},{}],[{artifacts:[artifact,artifact]},{}],
      [{parent:{...parent,status:'completed'}},{}],[{}, {SOURCE_REVISION:'f'.repeat(40)}],
      [{}, {OBSERVER_CONTEXT:JSON.stringify({...declared,parent_run_attempt:3})}]]) {
      const rejected = witness(changes,extraEnv);
      assert.equal(rejected.status,1);
      assert.doesNotMatch(rejected.stdout,/publication=authenticated/);
    }
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
      Date.now=()=> (fixture.now ?? 1100)*1000;
      globalThis.fetch = async (url, options = {}) => {
        const p = new URL(url).pathname;
        calls.push({method: options.method ?? 'GET', path: p});
        fs.writeFileSync(${JSON.stringify(callsFile)}, JSON.stringify(calls));
        if (p.endsWith('/cancel')) {cancelled = fixture.cancelStatus !== 403; return {status:fixture.cancelStatus ?? 202};}
        if (p.endsWith('/zip')) {
          const a = fixture.archives[p.includes('/501/') ? 0 : 1];
          return {ok:true,arrayBuffer:async()=>fs.readFileSync(a.archive)};
        }
        let record;
        if (p.endsWith('/42/attempts/2')) record=fixture.parent;
        else if (p.endsWith('/73/attempts/1/jobs')) record={total_count:1,jobs:[{
          id:91,run_id:73,run_attempt:1,head_sha:fixture.child.head_sha,status:'in_progress',conclusion:null,
          created_at:new Date(1000*1000).toISOString(),started_at:new Date(1010*1000).toISOString(),
          completed_at:null,runner_id:1,runner_name:'private-worker',
          steps:[{status:'in_progress',conclusion:null,started_at:new Date(1010*1000).toISOString()}],
        }]};
        else if (p.endsWith('/jobs')) {
          const jobs=[{id:801,run_id:42,run_attempt:2,head_sha:fixture.parent.head_sha,name:fixture.jobName,status:'completed',conclusion:'success',steps:fixture.steps},...(fixture.extraJobs??[])];
          record={total_count:jobs.length,jobs};
        }
        else if (p.endsWith('/artifacts')) {
          const index=p.includes('/42/') ? 0 : 1;
          const missing=fixture.missing === index;
          record={total_count:missing ? 0 : 1,artifacts:missing || (index === 1 && fixture.incompleteChildCensus)
            ? [] : [fixture.archives[index].metadata]};
        }
        else if (p.endsWith('/rehearsal.yml/runs')) {
          const selected = new URL(url).searchParams.get('status');
          const runs = selected === null ? [fixture.child,{...fixture.child,id:74,display_title:'ordinary reused success',status:'completed',conclusion:'success'}]
            : fixture.liveEmpty || selected !== fixture.child.status ? [] : [fixture.child];
          record = fixture.incompleteLiveCensus && selected !== null ? {total_count:1}
            : {total_count:runs.length,workflow_runs:runs};
        }
        else if (p.endsWith('/73')) record=cancelled ? {...fixture.child,status:'completed',conclusion:'cancelled'} : fixture.child;
        else throw new Error('fixture-private-url-unknown');
        return {ok:true,json:async()=>record};
      };`);
    const env = { PATH: process.env.PATH, NODE_OPTIONS: `--import=${preload}`,
      GITHUB_EVENT_PATH: eventFile, GITHUB_EVENT_NAME: 'workflow_run', GITHUB_REPOSITORY: context.parent_repository,
      GH_TOKEN: 'fixture-read', CANARY_OWNER: 'acme', CANARY_NAME: 'widget-host', CANARY_WORKFLOW: 'rehearsal.yml' };
    const entry = fileURLToPath(new URL('../ci/consumer-observer-reconcile.mjs', import.meta.url));
    const execute = (changes, observerMode = false, sweep = false) => {
      fs.writeFileSync(config, JSON.stringify({ parent, child, archives, jobName: 'Declare consumer observer',
        steps: [{ name: observerDeclarationStep, number:2, status: 'completed', conclusion: 'success' },
          {name:observerPublicationWitness(context),number:3,status:'completed',conclusion:'success'}], ...changes }));
      const run = spawnSync(process.execPath, [entry, ...(observerMode ? ['--observer'] : [])], {
        env: {...env,GITHUB_EVENT_NAME:sweep ? 'schedule' : env.GITHUB_EVENT_NAME,
          GITHUB_RUN_ID:'42',GITHUB_RUN_ATTEMPT:'2',GITHUB_SHA:parent.head_sha,
          OBSERVER_CONTEXT:JSON.stringify(context)}, encoding: 'utf8' });
      assert.doesNotMatch(run.stdout + run.stderr, /widget-host|\/73|\/74|run_id|fixture-private/);
      return { run, posts: JSON.parse(fs.readFileSync(callsFile, 'utf8')).filter(call => call.method === 'POST') };
    };
    const positive = execute({});
    assert.equal(positive.run.status, 0, positive.run.stderr);
    assert.match(positive.run.stdout, /discovered=1 owned=1 terminal=1 cancelled=1 pending=0 refused=0/);
    assert.deepEqual(positive.posts.map(call => call.path), ['/repos/acme/widget-host/actions/runs/73/cancel']);
    const liveParent = {...parent,status:'in_progress',conclusion:null};
    const declaration = {name:observerDeclarationStep,number:2,status:'completed',conclusion:'success'};
    const witness = {name:observerPublicationWitness(context),number:3,status:'completed',conclusion:'success'};
    const declared = [declaration,witness];
    const observed = {name:'Observe the first bounded window',number:4,status:'completed',conclusion:'failure'};
    const stoppedObserver = execute({parent:liveParent,steps:[...declared,observed]},true);
    assert.equal(stoppedObserver.run.status,1,stoppedObserver.run.stderr);
    assert.deepEqual(stoppedObserver.posts,[],'a failed read cannot retire a live measurement');
    assert.match(stoppedObserver.run.stdout,/cancelled=0 pending=1 refused=0/);
    const failedDispatch = execute({parent:liveParent,steps:[...declared,{...observed,name:observerDispatchStep}]},true);
    assert.equal(failedDispatch.run.status,1,failedDispatch.run.stderr);
    assert.deepEqual(failedDispatch.posts,[]);
    const expiredObserver = execute({parent:liveParent,steps:[...declared,observed],now:8210},true);
    assert.equal(expiredObserver.run.status,0,expiredObserver.run.stderr);
    assert.deepEqual(expiredObserver.posts.map(call=>call.path),['/repos/acme/widget-host/actions/runs/73/cancel']);
    const baseline = {id:802,run_id:42,run_attempt:2,head_sha:parent.head_sha,status:'completed',conclusion:'failure',
      steps:[declaration,{...witness,name:observerPublicationWitness({...context,source_revision:'d'.repeat(40),
        dispatch_nonce:'21234567-89ab-4cde-8fab-0123456789ab'})},observed]};
    const paired = execute({parent:liveParent,steps:[...declared,observed],extraJobs:[baseline],now:8210},true);
    assert.equal(paired.run.status,0,paired.run.stderr);
    assert.deepEqual(paired.posts.map(call=>call.path),['/repos/acme/widget-host/actions/runs/73/cancel']);
    const otherFailed = execute({parent:liveParent,steps:[...declared,{...observed,conclusion:'success'}],
      extraJobs:[baseline],now:8210},true);
    assert.equal(otherFailed.posts.length,0,'the other publisher stopping cannot end this observer');
    assert.match(otherFailed.run.stdout,/cancelled=0 pending=1 refused=0/);
    const absentWitness = execute({parent:liveParent,steps:[declaration,observed],extraJobs:[baseline],now:8210},true);
    assert.equal(absentWitness.run.status,1);
    assert.match(absentWitness.run.stderr,/observer_job_identity_unproven/);
    assert.equal(absentWitness.posts.length,0);
    const failedParent = execute({parent:{...parent,conclusion:'failure'}});
    assert.equal(failedParent.run.status,0,failedParent.run.stderr);
    assert.deepEqual(failedParent.posts,[],'completion cleanup also preserves a measurement after observer failure');
    assert.match(failedParent.run.stdout,/cleanup remains pending: pending=1 refused=0/);
    const retained = execute({parent:{...parent,conclusion:'failure'}},false,true);
    assert.equal(retained.run.status,0,retained.run.stderr);
    assert.deepEqual(retained.posts,[],'the scheduled pass must preserve the same in-budget child');
    assert.match(retained.run.stdout,/cleanup=sweep scanned=1 declarations=1 discovered=1 owned=1 terminal=0 cancelled=0 pending=1 refused=0/);
    const expiredRetained = execute({parent:{...parent,conclusion:'failure'},now:8210},false,true);
    assert.equal(expiredRetained.run.status,0,expiredRetained.run.stderr);
    assert.deepEqual(expiredRetained.posts.map(call=>call.path),['/repos/acme/widget-host/actions/runs/73/cancel'],
      'a later scheduled pass must enforce the original deadline without a second parent completion');
    const failedCancellation = execute({parent:{...parent,conclusion:'failure'},now:8210,cancelStatus:403},false,true);
    assert.equal(failedCancellation.run.status,1,'an actual cancellation failure remains red');
    assert.match(failedCancellation.run.stdout,/errors=1/);
    assert.deepEqual(failedCancellation.posts.map(call=>call.path),['/repos/acme/widget-host/actions/runs/73/cancel']);
    const emptySweep = execute({liveEmpty:true},false,true);
    assert.equal(emptySweep.run.status,0,emptySweep.run.stderr);
    assert.match(emptySweep.run.stdout,/scanned=0 declarations=0 discovered=0 owned=0 terminal=0 cancelled=0 pending=0 refused=0/);
    const partialSweep = execute({incompleteLiveCensus:true},false,true);
    assert.equal(partialSweep.run.status,1);
    assert.match(partialSweep.run.stderr,/reason=observer_census_unmeasured/);
    assert.deepEqual(partialSweep.posts,[],'a partial census cannot become a measured zero');
    for (const sweep of [false, true]) {
      const partialChild = execute({incompleteChildCensus:true},false,sweep);
      assert.equal(partialChild.run.status,1,'an incomplete child census remains an operational failure');
      assert.match(partialChild.run.stdout,/errors=1/);
      assert.deepEqual(partialChild.posts,[],'an incomplete child census cannot authorize cancellation');
    }
    for (const changes of [{missing:0},{missing:1},{child:{...child,run_attempt:2}},
      {child:{...child,actor:{login:'unrelated[bot]',type:'Bot'}}}]) {
      const unownedSweep = execute({...changes,now:8210},false,true);
      assert.equal(unownedSweep.run.status,0);
      assert.deepEqual(unownedSweep.posts,[],'a scheduled deadline still requires exact custody');
      assert.match(unownedSweep.run.stdout,/cleanup remains pending:/);
    }
    for(const step of [{...observed,conclusion:'success'}, {...observed,status:'in_progress',conclusion:null},
      {...observed,name:'Unrelated failed step'}, {...observed,number:1}]) {
      const needed = execute({parent:liveParent,steps:[...declared,step]},true);
      assert.equal(needed.posts.length,0,'a live observer must preserve its child');
      assert.match(needed.run.stdout,/cancelled=0 pending=1 refused=0/);
    }
    for (const changes of [{ missing: 1 }, { child: { ...child, run_attempt: 2 } },
      { child: { ...child, actor: { login: 'unrelated[bot]', type: 'Bot' } } }]) {
      const negative = execute(changes);
      assert.equal(negative.run.status, 0);
      assert.equal(negative.posts.length, 0);
      assert.match(negative.run.stdout,/cleanup remains pending:/);
    }
    for (const changes of [{ missing: 0 },
      { parent: { ...parent, head_repository: { full_name: 'unrelated/foreign-parent' } } }]) {
      const unprovenParent = execute(changes);
      assert.equal(unprovenParent.run.status, 1);
      assert.equal(unprovenParent.posts.length, 0);
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
