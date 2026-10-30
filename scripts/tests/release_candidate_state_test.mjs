import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync,spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {activePromotions,successfulCandidate,ghRead} from '../lib/release-candidate-state.mjs';
import {admitChild,archiveProducerBinding,qualifyArchiveProducer,observerRunName,
  observerDeclarationName,observerDeclarationStep,observerPublicationWitness,
  observerDispatchActor} from '../lib/consumer-observer.mjs';

const source='a'.repeat(40),policy='b'.repeat(40),head='c'.repeat(40);
const repository='burin-labs/harn';
const consumer={repository:'acme/widget-host',workflow:'.github/workflows/rehearsal.yml'};
const context={schema:'harn.consumer_observer.v2',parent_repository:repository,parent_run_id:'900',
  parent_run_attempt:1,parent_head_sha:policy,source_revision:source,started_at:1000,
  dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab'};
const parent={id:900,run_attempt:1,head_sha:policy,head_branch:'main',event:'workflow_dispatch',
  path:'.github/workflows/promote-release.yml',status:'in_progress',conclusion:null,
  repository:{full_name:repository},head_repository:{full_name:repository},
  display_title:`Promote release ${source}`,created_at:'2026-10-30T00:00:00Z'};
const producer={...parent,id:42,head_sha:source,path:'.github/workflows/build-release-binaries.yml',
  event:'push',status:'completed',conclusion:'failure'};
const targets=JSON.parse(execFileSync('bash',[new URL('../lib/candidate_archive_contract.sh',import.meta.url).pathname,
  '--targets-json'],{encoding:'utf8'}));
const jobs=['Resolve release context','Prepare CLI AOT payload','Assemble candidate manifest',
  'Consumer release rehearsal / Decide whether main moved','Release residual audit',
  ...targets.map(target=>`Build ${target}`),
  ...['linux','linux-arm64','windows','macos'].map(target=>`Release smoke / Release smoke (${target})`)]
  .map((name,index)=>({id:index+1,name,run_id:42,run_attempt:1,head_sha:source,status:'completed',conclusion:'success'}));
const artifacts=[...targets.map(target=>`harn-${target}`),`candidate-manifest-${source}`,'harn-release-files']
  .map((name,index)=>({id:index+100,name,expired:false,size_in_bytes:100,digest:`sha256:${'d'.repeat(64)}`,
    workflow_run:{id:42,head_sha:source}}));
const child={id:73,run_attempt:1,head_sha:head,head_branch:'repair',head_repository:{full_name:consumer.repository},
  repository:{full_name:consumer.repository},path:consumer.workflow,event:'workflow_dispatch',status:'in_progress',
  conclusion:null,actor:{login:observerDispatchActor,type:'Bot'},display_title:observerRunName(context)};
const parentJobs=[{id:901,run_id:900,run_attempt:1,head_sha:policy,name:'Consumer canary',status:'in_progress',
  conclusion:null,steps:[{name:observerDeclarationStep,number:1,status:'completed',conclusion:'success'},
    {name:observerPublicationWitness(context),number:2,status:'completed',conclusion:'success'}]}];

function fixture(changes={}){
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-recovery-state-'));
  const metadata=(id,name,runId,sha,value)=>{
    const filename=id===500?'consumer-observer.json':`consumer-observer-${context.dispatch_nonce}.json`;
    const input=path.join(scratch,filename),zip=path.join(scratch,`${id}.zip`);
    fs.writeFileSync(input,JSON.stringify(value));
    execFileSync('zip',['-j',zip,input]);
    const bytes=fs.readFileSync(zip);
    return {bytes,artifact:{id,name,expired:false,size_in_bytes:bytes.length,
      digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,workflow_run:{id:runId,head_sha:sha}}};
  };
  const binding=archiveProducerBinding(qualifyArchiveProducer(producer,jobs,artifacts,
    {sourceRevision:source,runId:'42',attempt:1},targets));
  const expected={...consumer,runId:'73',attempt:1,headSha:head,sourceRevision:source,producerBinding:binding};
  const receipt=admitChild(context,parent,expected,child,{inputs:{observer:JSON.stringify(context),
    source_revision:source,release_producer_run_id:'42',release_producer_run_attempt:'1'}},context);
  const declaration=metadata(500,observerDeclarationName(context),900,policy,changes.context??context);
  const ownership=metadata(501,`consumer-observer-${context.dispatch_nonce}`,73,head,changes.receipt??receipt);
  const calls=[];
  let parentReads=0;
  const read=async(url)=>{
    const endpoint=new URL(url),route=endpoint.pathname;
    calls.push(route+endpoint.search);
    let value;
    if(route.endsWith('/zip'))return new Response(route.includes('/500/')?declaration.bytes:ownership.bytes);
    if(route.includes('/workflows/promote-release.yml/runs')){
      const rows=endpoint.searchParams.get('status')==='in_progress'?(changes.promotions??[parent]):[];
      const page=Number(endpoint.searchParams.get('page'));
      value={total_count:rows.length,...changes.promotionTotal,workflow_runs:rows.slice((page-1)*100,page*100)};
    }else if(route.includes('/workflows/build-release-binaries.yml/runs')){
      const rows=changes.producers??[producer],page=Number(endpoint.searchParams.get('page'));
      value={total_count:rows.length,...changes.producerTotal,workflow_runs:rows.slice((page-1)*100,page*100)};
    }else if(route.endsWith('/workflows/rehearsal.yml/runs'))value={total_count:1,workflow_runs:[child]};
    else if(route.endsWith('/900/artifacts'))value={total_count:changes.missingWitness?0:1,
      artifacts:changes.missingWitness?[]:[declaration.artifact]};
    else if(route.endsWith('/42/artifacts')){
      const rows=endpoint.searchParams.has('name')?artifacts.filter(item=>item.name.startsWith('candidate-manifest-')):artifacts;
      value={total_count:rows.length,artifacts:rows};
    }else if(route.endsWith('/73/artifacts'))value={total_count:1,artifacts:[ownership.artifact]};
    else if(route.endsWith('/900/attempts/1/jobs'))value={total_count:1,jobs:changes.parentJobs??parentJobs};
    else if(route.endsWith('/42/attempts/1/jobs'))value={total_count:jobs.length,jobs:changes.jobs??jobs};
    else if(route.endsWith('/73/attempts/1/jobs'))value={total_count:1,jobs:[{id:74,run_id:73,run_attempt:1,
      head_sha:head,name:'Rehearse',status:'in_progress',conclusion:null,
      created_at:new Date(1000*1000).toISOString(),started_at:new Date(1010*1000).toISOString(),
      completed_at:null,runner_id:1,runner_name:'fixture',steps:[{status:'in_progress',conclusion:null,
        started_at:new Date(1010*1000).toISOString()}]}]};
    else if(/\/runs\/900(?:\/attempts\/1)?$/.test(route)){
      parentReads++;
      value=changes.finalParent&&parentReads>1?changes.finalParent:changes.parent??parent;
    }
    else if(/\/runs\/42(?:\/attempts\/1)?$/.test(route))value=changes.producer??producer;
    else if(route.endsWith('/runs/73'))value=changes.child??child;
    else throw Error(`Unexpected fixture route ${route}`);
    return new Response(JSON.stringify(value));
  };
  return {read,calls,close:()=>fs.rmSync(scratch,{recursive:true,force:true})};
}
async function recovery(changes={}){
  const data=fixture(changes);
  try{return await activePromotions('public','private',repository,'0',consumer,data.read,()=>1040);}
  finally{data.close();}
}
test('failed overall producer counts only through actual archive and nonce-bound live consumer custody',async()=>{
  const result=await recovery();
  assert.deepEqual(result.promoting,['900']);
  assert.equal(result.recoveries[0].producer_run,'42');
  assert.equal(result.recoveries[0].owned_consumers,1);
  assert.equal(result.recoveries[0].pending_consumer_jobs,1);
  assert.deepEqual(result.pending_jobs,['Consumer canary']);
  assert.deepEqual(result.bad_jobs,[]);
});
test('ordinary selector remains success-only and measures complete source inventory',async()=>{
  for(const conclusion of ['failure','startup_failure','success']){
    const data=fixture({producers:[{...producer,conclusion}]});
    try{assert.equal(await successfulCandidate('public',repository,source,data.read),conclusion==='success'?'42':'');}
    finally{data.close();}
  }
  const data=fixture({producerTotal:{total_count:2}});
  try{await assert.rejects(successfulCandidate('public',repository,source,data.read),/census_unmeasured/);}
  finally{data.close();}
  const late=fixture({producers:[...Array.from({length:100},(_,index)=>({...producer,id:index+1000})),
    {...producer,conclusion:'success'}]});
  try{
    assert.equal(await successfulCandidate('public',repository,source,late.read),'42');
    assert.ok(late.calls.some(call=>call.includes('page=2')));
  }finally{late.close();}
});
test('known successful producer retains ordinary promotion path without private recovery authority',async()=>{
  const data=fixture({producers:[{...producer,conclusion:'success'}],missingWitness:true});
  try{
    const result=await activePromotions('public',null,repository,'0',null,data.read,()=>1040);
    assert.deepEqual(result.promoting,['900']);
    assert.deepEqual(result.recoveries,[]);
    assert.ok(!data.calls.some(call=>call.includes('widget-host')));
  }finally{data.close();}
});
test('unknown and partial fresh parent states refuse on initial and final reads',async()=>{
  for(const state of [{status:''},{status:'unreported'},{status:'in_progress',conclusion:undefined},
    {status:'completed',conclusion:undefined}]){
    await assert.rejects(recovery({parent:{...parent,...state}}),/promotion_state_unmeasured/);
    await assert.rejects(recovery({producers:[{...producer,conclusion:'success'}],
      finalParent:{...parent,...state}}),/promotion_state_unmeasured/);
  }
  await assert.rejects(recovery({finalParent:{...parent,status:'unreported'}}),/CONSUMER_OBSERVER/);
});
test('missing, foreign and replayed publisher witnesses never suppress a candidate',async()=>{
  await assert.rejects(recovery({missingWitness:true}),/declaration_unmeasured/);
  for(const change of [{source_revision:'e'.repeat(40)},{parent_run_attempt:2},{parent_run_id:'899'}]){
    await assert.rejects(recovery({context:{...context,...change}}),/CONSUMER_OBSERVER/);
  }
  await assert.rejects(recovery({parentJobs:parentJobs.map(job=>({...job,steps:job.steps.slice(0,1)}))}),/observer_job_identity_unproven/);
});
test('failed build, ended recovery and failed consumer do not count as active promotion',async()=>{
  const failed=await recovery({jobs:jobs.map(job=>job.name.startsWith('Build ')?{...job,conclusion:'failure'}:job)});
  assert.deepEqual(failed.promoting,[]);
  assert.deepEqual(failed.bad_jobs,targets.map(target=>`Build ${target}`));
  assert.deepEqual((await recovery({parent:{...parent,status:'completed',conclusion:'failure'}})).promoting,[]);
  assert.deepEqual((await recovery({parent:{...parent,status:'completed',conclusion:'startup_failure'}})).promoting,[]);
  assert.deepEqual((await recovery({child:{...child,status:'completed',conclusion:'failure'}})).promoting,[]);
});
test('foreign archive binding cannot qualify an otherwise owned live consumer',async()=>{
  const binding=archiveProducerBinding(qualifyArchiveProducer(producer,jobs,artifacts,
    {sourceRevision:source,runId:'42',attempt:1},targets));
  const receipt=admitChild(context,parent,{...consumer,runId:'73',attempt:1,headSha:head,
    sourceRevision:source,producerBinding:binding},child,{inputs:{observer:JSON.stringify(context),
      source_revision:source,release_producer_run_id:'42',release_producer_run_attempt:'1'}},context);
  receipt.producer.producer_run='43';
  assert.deepEqual((await recovery({receipt})).promoting,[]);
});
test('partial inventory and unknown titles fail closed; active promotion beyond first page is measured',async()=>{
  await assert.rejects(recovery({promotionTotal:{total_count:2}}),/census_unmeasured/);
  await assert.rejects(recovery({promotions:[{...parent,display_title:'Unknown promotion'}]}),/promotion_source_unmeasured/);
  const ended=Array.from({length:100},(_,i)=>({...parent,id:i+1000,status:'in_progress'}));
  const data=fixture({promotions:[...ended,parent]});
  try{
    const result=await activePromotions('public','private',repository,'0',consumer,async(url,options)=>{
      const old=/\/runs\/(1[0-9]{3})$/.exec(new URL(url).pathname);
      if(old)return new Response(JSON.stringify({...parent,id:Number(old[1]),status:'completed',conclusion:'success'}));
      return data.read(url,options);
    },()=>1040);
    assert.deepEqual(result.promoting,['900']);
    assert.ok(data.calls.some(call=>call.includes('page=2')));
  }finally{data.close();}
});
test('gh read transport preserves ZIP bytes and refuses mutation',async()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-gh-bytes-'));
  const previous=process.env.PATH;
  try{
    fs.writeFileSync(path.join(scratch,'gh'),'#!/usr/bin/env node\nprocess.stdout.write(Buffer.from([0,255,128,65]));\n',{mode:0o700});
    process.env.PATH=`${scratch}:${previous}`;
    const response=await ghRead('https://api.github.com/repos/acme/test/actions/artifacts/1/zip');
    assert.deepEqual(Buffer.from(await response.arrayBuffer()),Buffer.from([0,255,128,65]));
    await assert.rejects(ghRead('https://api.github.com/repos/acme/test',{method:'POST'}),/read_endpoint_invalid/);
  }finally{process.env.PATH=previous;fs.rmSync(scratch,{recursive:true,force:true});}
});
test('actual source reuse measures run and artifact page two and refuses either partial inventory',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-source-reuse-'));
  try{
    const runs=[...Array.from({length:100},(_,index)=>({...producer,id:1000+index})),
      {...producer,conclusion:'success'}];
    const record=path.join(scratch,'fixture.json'),calls=path.join(scratch,'calls');
    const available=[...Array.from({length:100},(_,index)=>`other-${index}`),
      'harn-a','harn-b',`candidate-manifest-${source}`,'harn-release-files']
      .map((name,index)=>({id:index+1,name,expired:false,size_in_bytes:1}));
    fs.writeFileSync(record,JSON.stringify({runs,available,partial:false}));
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
import fs from 'node:fs';
const endpoint=new URL('https://api.github.com/'+process.argv[3]);
const data=JSON.parse(fs.readFileSync(process.env.FIXTURE,'utf8'));
fs.appendFileSync(process.env.CALLS,endpoint.pathname+endpoint.search+'\\n');
if(endpoint.pathname.includes('/workflows/')){
  const page=Number(endpoint.searchParams.get('page'));
  console.log(JSON.stringify({total_count:data.runs.length+(data.partial?1:0),workflow_runs:data.runs.slice((page-1)*100,page*100)}));
}else {
  const page=Number(endpoint.searchParams.get('page'));
  console.log(JSON.stringify({total_count:data.available.length+(data.partialArtifacts?1:0),artifacts:data.available.slice((page-1)*100,page*100)}));
}
`,{mode:0o700});
    const entry=path.join(scratch,'reuse.sh');
    fs.writeFileSync(entry,`#!/usr/bin/env bash\nset -euo pipefail\nsource '${fileURLToPath(new URL('../lib/release_candidate_run.sh',import.meta.url))}'\nsource_candidate_live_run_id burin-labs/harn '${source}' '[{"target":"a"},{"target":"b"}]'\n`);
    const options={encoding:'utf8',env:{...process.env,PATH:`${scratch}:${process.env.PATH}`,
      FIXTURE:record,CALLS:calls,GH_TOKEN:'fixture-read'}};
    const admitted=spawnSync('bash',[entry],options);
    assert.equal(admitted.status,0,admitted.stderr);
    assert.equal(admitted.stdout.trim(),'42');
    assert.match(fs.readFileSync(calls,'utf8'),/page=2/);
    assert.match(fs.readFileSync(calls,'utf8'),/\/runs\/42\/artifacts\?per_page=100&page=2/);
    fs.writeFileSync(record,JSON.stringify({runs,available,partial:true}));
    const partial=spawnSync('bash',[entry],options);
    assert.equal(partial.status,1);
    assert.equal(partial.stdout,'');
    assert.match(partial.stderr,/census_unmeasured/);
    fs.writeFileSync(record,JSON.stringify({runs,available,partial:false,partialArtifacts:true}));
    const partialArtifacts=spawnSync('bash',[entry],options);
    assert.equal(partialArtifacts.status,1);
    assert.equal(partialArtifacts.stdout,'');
    assert.match(partialArtifacts.stderr,/census_unmeasured/);
    fs.writeFileSync(record,JSON.stringify({runs:runs.map(row=>({...row,head_branch:undefined})),available}));
    const unmeasuredBranch=spawnSync('bash',[entry],options);
    assert.equal(unmeasuredBranch.status,1);
    assert.equal(unmeasuredBranch.stdout,'');
    assert.match(unmeasuredBranch.stderr,/source_census_unmeasured/);
  }finally{fs.rmSync(scratch,{recursive:true,force:true});}
});
