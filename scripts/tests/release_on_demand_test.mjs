import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {Evaluator,Lexer,Parser,data} from '@actions/expressions';
import {load} from 'js-yaml';
import {releaseRequest,requestsForMainPush,releaseObservation,runRequestedRelease,reportRequestedRelease,releaseReport} from '../lib/release-on-demand.mjs';

const repository='octo-org/language',merge='a'.repeat(40),source='b'.repeat(40),newSource='c'.repeat(40);
const request={repository,pr:17,merge_sha:merge,source_sha:source};
const controller='https://github.com/octo-org/language/actions/runs/100';
function fixture(options={}) {
  const dispatches=[],writes=[],progress=[];
  const run=(id)=>({id,run_attempt:1,event:'workflow_dispatch',head_branch:'main',
    repository:{full_name:repository},head_repository:{full_name:repository},
    path:`.github/workflows/${id===42?'build-release-binaries':'promote-release'}.yml`,head_sha:options.newSource??source,
    display_title:id===42?'Build release binaries [source-candidate]':`Promote release ${options.newSource??source}`,
    status:'completed',conclusion:'success',...options.run,...(options.runs?.[id]??{})});
  const jobs=(id)=>options.jobs?.[id]??[{id:id+100,run_id:id,run_attempt:1,head_sha:run(id).head_sha,
    name:id===42?'Consumer release rehearsal / Consumer canary':'Promote / Publish',status:'completed',conclusion:'success'}];
  const api={token:'fixture',
    async read(endpoint) {
      if(endpoint.endsWith('/pulls/17'))return {number:17,state:'closed',merged:true,merge_commit_sha:merge,
        base:{ref:'main',repo:{full_name:repository}},labels:[{name:'release-needed'}],...options.pull};
      if(endpoint.endsWith('/git/ref/heads/main'))return {object:{sha:source}};
      if(endpoint.includes('/compare/'))return {status:'ahead',base_commit:{sha:merge},merge_base_commit:{sha:merge},...options.comparison};
      throw new Error(`unexpected fixture read ${endpoint}`);
    },
    async fetch(url) {
      const endpoint=new URL(url);
      const id=Number(/\/runs\/(\d+)/.exec(endpoint.pathname)?.[1]);
      if(![42,43].includes(id))throw new Error(`unexpected fixture URL ${url}`);
      return new Response(JSON.stringify(endpoint.pathname.endsWith('/jobs')?
        {total_count:jobs(id).length,jobs:jobs(id),...options.census}:run(id)));
    },
    async dispatch(workflow,inputs) {
      dispatches.push({workflow,inputs});
      if(options.dispatchFailure)throw new Error('github_write_unmeasured');
      const id=dispatches.length===1?42:43;
      return options.dispatchResponse??{workflow_run_id:id,run_url:`https://api.github.com/repos/${repository}/actions/runs/${id}`,
        html_url:`https://github.com/${repository}/actions/runs/${id}`};
    },
    async qualify(id,sha) {
      assert.equal(id,'42');assert.equal(sha,options.newSource??source);
      if(options.bindingFailure)throw new Error('rehearsal_binding_identity_mismatch');
    },
    async list(endpoint) {return endpoint.includes('/comments')?options.comments??[]:options.issues??[];},
    async write(endpoint,body,method) {writes.push({endpoint,body,method});},
    async ensureBlockedLabel(){writes.push({label:'release-blocked'});},
  };
  let clock=0;
  const execution={now:()=>clock,sleep:async ms=>{clock+=ms;},progress:async state=>progress.push(structuredClone(state))};
  return {api,dispatches,writes,progress,execution};
}

test('merged labeled request dispatches exact fresh candidate then exact-source promotion',async()=>{
  const f=fixture({newSource});
  const admitted=await releaseRequest(f.api,repository,17);
  const result=await runRequestedRelease(f.api,admitted,f.execution);
  assert.equal(result.verdict,'complete');
  assert.deepEqual(f.dispatches,[{workflow:'build-release-binaries.yml',inputs:{source_candidate:true}},
    {workflow:'promote-release.yml',inputs:{candidate_sha:newSource,candidate_run_id:'42'}}]);
  assert(f.progress.some(row=>row.phase==='promotion'&&row.promotion_run_id===43));
  assert.equal(result.observed_jobs,1);assert.deepEqual(result.pending_jobs,[]);assert.deepEqual(result.failing_jobs,[]);
});

for(const [name,options] of Object.entries({unmerged:{pull:{merged:false}},unlabeled:{pull:{labels:[]}},
  foreignBase:{pull:{base:{ref:'main',repo:{full_name:'another/repo'}}}},behind:{comparison:{status:'behind'}},
  foreignMergeBase:{comparison:{merge_base_commit:{sha:source}}}})) {
  test(`refuse ${name} before any dispatch`,async()=>{
    const f=fixture(options);await assert.rejects(releaseRequest(f.api,repository,17));assert.equal(f.dispatches.length,0);
  });
}

test('dry run reads the actual request and plans both owner dispatches without writes',async()=>{
  const f=fixture({pull:{labels:[]}});
  const admitted=await releaseRequest(f.api,repository,17,true);
  const result=await runRequestedRelease(f.api,admitted,{...f.execution,dryRun:true});
  assert.equal(result.verdict,'dry-run');assert.equal(result.dispatch_plan.length,2);
  assert.equal(f.dispatches.length,0);assert.equal(f.writes.length,0);
});

for(const [name,options] of Object.entries({
  empty:{jobs:{42:[]}},partial:{census:{total_count:2}},
  foreignJob:{jobs:{42:[{id:1,run_id:90,run_attempt:1,head_sha:source,name:'Build',status:'completed',conclusion:'success'}]}},
  skippedOnly:{jobs:{42:[{id:1,run_id:42,run_attempt:1,head_sha:source,name:'Build',status:'completed',conclusion:'skipped'}]}},
  partialRerun:{run:{run_attempt:2}},foreignRun:{run:{head_repository:{full_name:'another/repo'}}},
  warm:{run:{display_title:'Build release binaries [warm]'}},unknown:{run:{status:'unexpected'}},
  binding:{bindingFailure:true},missingDispatchId:{dispatchResponse:{}},dispatchFailed:{dispatchFailure:true},
})) {
  test(`${name} cannot dispatch promotion or report release completion`,async()=>{
    const f=fixture(options);const result=await runRequestedRelease(f.api,request,f.execution);
    assert.equal(result.verdict,'blocked');assert.equal(f.dispatches.length,1);
    assert(result.pending_jobs.length+result.failing_jobs.length>0);
  });
}

test('failed job names survive alongside pending names',async()=>{
  const f=fixture({runs:{42:{conclusion:'failure'}},jobs:{42:[
    {id:1,run_id:42,run_attempt:1,head_sha:source,name:'Build platform',status:'completed',conclusion:'failure'},
    {id:2,run_id:42,run_attempt:1,head_sha:source,name:'Rehearse',status:'in_progress',conclusion:null}]}});
  const result=await runRequestedRelease(f.api,request,f.execution);
  assert.equal(result.verdict,'blocked');assert.deepEqual(result.failing_jobs,['Build platform']);
  assert.deepEqual(result.pending_jobs,['Rehearse']);assert.equal(f.dispatches.length,1);
});

test('a stalled candidate exhausts its deadline using a virtual clock',async()=>{
  const f=fixture({runs:{42:{status:'queued',conclusion:null}},jobs:{42:[]}});
  const result=await runRequestedRelease(f.api,request,f.execution);
  assert.equal(result.reason,'candidate_deadline_exceeded');assert.deepEqual(result.pending_jobs,['candidate jobs have not reported']);
  assert.equal(f.dispatches.length,1);
});

test('a failed promotion remains blocked after a successful candidate',async()=>{
  const f=fixture({runs:{43:{conclusion:'failure'}},jobs:{43:[{id:5,run_id:43,run_attempt:1,head_sha:source,
    name:'Publish qualified files',status:'completed',conclusion:'failure'}]}});
  const result=await runRequestedRelease(f.api,request,f.execution);
  assert.equal(result.verdict,'blocked');assert.deepEqual(result.failing_jobs,['Publish qualified files']);
  assert.equal(f.dispatches.length,2);
});

test('a known failed job is reported immediately while the rest of its workflow is running',async()=>{
  const f=fixture({runs:{42:{status:'in_progress',conclusion:null}},jobs:{42:[
    {id:1,run_id:42,run_attempt:1,head_sha:source,name:'Build',status:'completed',conclusion:'failure'}]}});
  const result=await runRequestedRelease(f.api,request,f.execution);
  assert.equal(result.verdict,'blocked');assert.deepEqual(result.failing_jobs,['Build']);assert.equal(f.dispatches.length,1);
});

test('blocked requests update one owned PR comment and issue; successful recovery closes it',async()=>{
  const marker='<!-- harn.release-on-demand.v1 pr=17 -->';
  const f=fixture({comments:[{id:7,body:marker,user:{login:'github-actions[bot]'}},
    {id:8,body:marker,user:{login:'untrusted'}}],issues:[{id:9,number:19,body:marker,user:{login:'github-actions[bot]'}}]});
  const state={...request,phase:'candidate',verdict:'blocked',observed_jobs:1,pending_jobs:['Rehearse'],failing_jobs:['Build']};
  await reportRequestedRelease(f.api,state,controller);
  assert(f.writes.some(row=>row.endpoint?.endsWith('/issues/comments/7')&&row.method==='PATCH'));
  assert(f.writes.some(row=>row.endpoint?.endsWith('/issues/19')&&row.method==='PATCH'&&row.body.state==='open'));
  assert(releaseReport(state,controller).includes('Pending (1): Rehearse. Failing (1): Build.'));
  await reportRequestedRelease(f.api,{...state,verdict:'complete',pending_jobs:[],failing_jobs:[]},controller);
  assert(f.writes.some(row=>row.endpoint?.endsWith('/issues/19')&&row.body.state==='closed'));
});

test('live-reader positive and missing census controls use the same exact-attempt path',async()=>{
  assert.equal((await releaseObservation(fixture().api,request,'candidate',42)).verdict,'passed');
  assert.equal((await releaseObservation(fixture({jobs:{42:[]}}).api,request,'candidate',42)).verdict,'blocked');
});

test('the deployed caller admits only trusted main and uses a typed merged-request selector',async()=>{
  const workflow=load(fs.readFileSync(new URL('../../.github/workflows/release-on-demand.yml',import.meta.url),'utf8'));
  const expressionValue=value=>value==null?new data.Null():typeof value==='string'?new data.StringData(value):
    typeof value==='boolean'?new data.BooleanData(value):Array.isArray(value)?new data.Array(...value.map(expressionValue)):
      new data.Dictionary(...Object.entries(value).map(([key,value])=>({key,value:expressionValue(value)})));
  const evaluate=github=>{
    const tokens=new Lexer(workflow.jobs.release.if).lex();
    return new Evaluator(new Parser(tokens.tokens,['github'],[]).parse(),
      expressionValue({github})).evaluate().coerceString()==='true';
  };
  const context={ref:'refs/heads/main',event_name:'push'};
  assert.equal(evaluate(context),true);
  assert.equal(evaluate({...context,ref:'refs/heads/topic'}),false);
  assert.equal(evaluate({ref:'refs/heads/main',event_name:'workflow_dispatch',event:{}}),true);
  assert.equal(workflow.on.pull_request_target,undefined);
  assert(workflow.on.workflow_call);
  assert.equal(workflow.on.workflow_dispatch.inputs.dry_run.default,true);
  assert.equal(workflow.on.schedule,undefined);
  assert(workflow.jobs.report.if.includes('always()'));
  for(const job of Object.values(workflow.jobs)) {
    const checkout=job.steps.find(step=>step.uses?.startsWith('actions/checkout@'));
    assert.equal(checkout.with.ref,'main');assert.equal(checkout.with['persist-credentials'],false);
  }
  const trigger=load(fs.readFileSync(new URL('../../.github/workflows/release-needed.yml',import.meta.url),'utf8'));
  assert.deepEqual(trigger.on,{push:{branches:['main']}});
  assert.equal(trigger.jobs.release.uses,'./.github/workflows/release-on-demand.yml');
  assert.equal(trigger.jobs.release.with.dry_run,false);
  const pull={id:1,number:17,state:'closed',merged_at:'2026-11-11T00:00:00Z',merge_commit_sha:source,
    labels:[{name:'release-needed'}],base:{ref:'main',repo:{full_name:repository}},head:{repo:{full_name:'contributor/fork'}}};
  const api={list:async()=>[pull]};
  assert.deepEqual(await requestsForMainPush(api,repository,source),[17]);
  for(const change of [{state:'open',merged_at:null},{labels:[]},{merge_commit_sha:merge},{base:{ref:'other',repo:{full_name:repository}}}]) {
    assert.deepEqual(await requestsForMainPush({list:async()=>[{...pull,...change}]},repository,source),[]);
  }
  await assert.rejects(requestsForMainPush({list:async()=>[{...pull,labels:null}]},repository,source));
});

test('interrupted controller converts its pending receipt to blocked before notification',()=>{
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'harn-release-request-'));
  try {
    const filename=path.join(root,'report.json');
    fs.writeFileSync(filename,JSON.stringify({schema:'harn.release_on_demand.v1',...request,phase:'candidate',verdict:'pending',observed_jobs:1,pending_jobs:['Build'],failing_jobs:[]}));
    const child=spawnSync(process.execPath,[new URL('../release-on-demand.mjs',import.meta.url).pathname,'report'],
      {encoding:'utf8',env:{...process.env,GITHUB_REPOSITORY:repository,RELEASE_REQUEST_PR:'17',RELEASE_REQUEST_DRY_RUN:'true',RELEASE_REQUEST_REPORT:filename}});
    assert.equal(child.status,0,child.stderr);
    const result=JSON.parse(child.stdout);assert.equal(result.verdict,'blocked');assert.equal(result.reason,'controller_failed_or_timed_out');
    assert.deepEqual(result.pending_jobs,['Build']);
  } finally {fs.rmSync(root,{recursive:true,force:true});}
});

test('a PR notification failure still attempts the blocked issue and fails loudly',async()=>{
  const f=fixture();
  const write=f.api.write;
  f.api.write=async(endpoint,...args)=>{
    if(endpoint.endsWith('/comments'))throw new Error('fixture comment unavailable');
    return write(endpoint,...args);
  };
  await assert.rejects(reportRequestedRelease(f.api,{...request,verdict:'blocked',phase:'candidate',observed_jobs:1,
    pending_jobs:[],failing_jobs:['Build']},controller),/release_notification_incomplete/);
  assert(f.writes.some(row=>row.endpoint?.endsWith('/issues')&&row.body.title==='Release blocked for #17'));
});
