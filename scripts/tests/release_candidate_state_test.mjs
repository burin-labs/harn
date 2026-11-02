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
    const filename=id===500?'consumer-observer.json':id===502?'candidate-manifest.json':
      `consumer-observer-${context.dispatch_nonce}.json`;
    const input=path.join(scratch,filename),zip=path.join(scratch,`${id}.zip`);
    fs.writeFileSync(input,JSON.stringify(value));
    execFileSync('zip',['-j',zip,input]);
    const bytes=fs.readFileSync(zip);
    return {bytes,artifact:{id,name,expired:false,size_in_bytes:bytes.length,
      digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,workflow_run:{id:runId,head_sha:sha}}};
  };
  const manifest=metadata(502,`candidate-manifest-${source}`,42,source,
    {schemaVersion:'burin-labs.candidate_manifest.v1',repository,sourceCommit:source,runId:'42',runAttempt:'1'});
  const measured=artifacts.map(item=>item.name===manifest.artifact.name?manifest.artifact:item);
  const binding=archiveProducerBinding(qualifyArchiveProducer(producer,jobs,measured,
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
    if(route.endsWith('/zip'))return new Response(route.includes('/500/')?declaration.bytes:
      route.includes('/502/')?manifest.bytes:ownership.bytes);
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
      const rows=endpoint.searchParams.has('name')?measured.filter(item=>item.name.startsWith('candidate-manifest-')):measured;
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
    else if(/\/runs\/42(?:\/attempts\/1)?$/.test(route))value=changes.producer??
      changes.producers?.find(item=>item.id===42)??producer;
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
test('unbound run and artifact CLI selectors cannot be used for release decisions',()=>{
  const reader=fileURLToPath(new URL('../lib/release-candidate-state.mjs',import.meta.url));
  for(const operation of ['successful-main-runs','artifacts']){
    const result=spawnSync(process.execPath,[reader,operation,repository,source],{encoding:'utf8'});
    assert.notEqual(result.status,0);
    assert.match(result.stderr,/operation_invalid/);
  }
});
test('actual source reuse measures run and artifact page two and refuses either partial inventory',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-source-reuse-'));
  try{
    const runs=[...Array.from({length:100},(_,index)=>({...producer,id:1000+index})),
      {...producer,conclusion:'success'}];
    const record=path.join(scratch,'fixture.json'),calls=path.join(scratch,'calls');
    const available=[...Array.from({length:100},(_,index)=>`other-${index}`),
      'harn-a','harn-b',`candidate-manifest-${source}`,'harn-release-files']
      .map((name,index)=>({id:index+1,name,expired:false,size_in_bytes:1,
        digest:`sha256:${'d'.repeat(64)}`,workflow_run:{id:42,head_sha:source}}));
    const manifest={schemaVersion:'burin-labs.candidate_manifest.v1',repository,sourceCommit:source,
      runId:'42',runAttempt:'1',artifacts:['a','b'].map(target=>({kind:'archive',target,
        artifact:`harn-${target}`,sha256:'d'.repeat(64)}))};
    manifest.artifacts.push({kind:'notes',artifact:'harn-release-files',sha256:'e'.repeat(64)});
    const manifestPath=path.join(scratch,'candidate-manifest.json'),archive=path.join(scratch,'manifest.zip');
    fs.writeFileSync(manifestPath,JSON.stringify(manifest));
    execFileSync('zip',['-j',archive,manifestPath]);
    const bytes=fs.readFileSync(archive),entry=available.find(item=>item.name===`candidate-manifest-${source}`);
    entry.size_in_bytes=bytes.length;
    entry.digest=`sha256:${createHash('sha256').update(bytes).digest('hex')}`;
    fs.writeFileSync(record,JSON.stringify({runs,available,partial:false}));
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
import fs from 'node:fs';
const endpoint=new URL('https://api.github.com/'+process.argv[3]);
const data=JSON.parse(fs.readFileSync(process.env.FIXTURE,'utf8'));
fs.appendFileSync(process.env.CALLS,endpoint.pathname+endpoint.search+'\\n');
if(endpoint.pathname.includes('/workflows/')){
  const page=Number(endpoint.searchParams.get('page'));
  console.log(JSON.stringify({total_count:data.runs.length+(data.partial?1:0),workflow_runs:data.runs.slice((page-1)*100,page*100)}));
}else if(endpoint.pathname.endsWith('/runs/42')){
  console.log(JSON.stringify(data.runs.at(-1)));
}else if(endpoint.pathname.endsWith('/runs/42/attempts/1')){
  console.log(JSON.stringify(data.runs.at(-1)));
}else if(endpoint.pathname.endsWith('/attempts/1/jobs')){
  const jobs=['Build a','Build b','Assemble candidate manifest'].map((name,index)=>({id:500+index,name,
    run_id:42,run_attempt:1,head_sha:'${source}',status:'completed',conclusion:'success'}));
  console.log(JSON.stringify({total_count:jobs.length,jobs}));
}else if(endpoint.pathname.endsWith('/artifacts/103/zip')){
  process.stdout.write(fs.readFileSync(process.env.MANIFEST_ZIP));
}else if(endpoint.pathname.endsWith('/artifacts')){
  const page=Number(endpoint.searchParams.get('page'));
  console.log(JSON.stringify({total_count:data.available.length+(data.partialArtifacts?1:0),artifacts:data.available.slice((page-1)*100,page*100)}));
}else process.exit(1);
`,{mode:0o700});
    const script=path.join(scratch,'reuse.sh');
    fs.writeFileSync(script,`#!/usr/bin/env bash\nset -euo pipefail\nsource '${fileURLToPath(new URL('../lib/release_candidate_run.sh',import.meta.url))}'\nsource_candidate_live_run_id burin-labs/harn '${source}' '[{"target":"a"},{"target":"b"}]'\n`);
    const options={encoding:'utf8',env:{...process.env,PATH:`${scratch}:${process.env.PATH}`,
      FIXTURE:record,CALLS:calls,MANIFEST_ZIP:archive,GH_TOKEN:'fixture-read'}};
    const admitted=spawnSync('bash',[script],options);
    assert.equal(admitted.status,0,admitted.stderr);
    assert.equal(admitted.stdout.trim(),'42');
    assert.match(fs.readFileSync(calls,'utf8'),/page=2/);
    assert.match(fs.readFileSync(calls,'utf8'),/\/runs\/42\/artifacts\?per_page=100&page=2/);
    fs.writeFileSync(record,JSON.stringify({runs,available,partial:true}));
    const partial=spawnSync('bash',[script],options);
    assert.equal(partial.status,1);
    assert.equal(partial.stdout,'');
    assert.match(partial.stderr,/census_unmeasured/);
    fs.writeFileSync(record,JSON.stringify({runs,available,partial:false,partialArtifacts:true}));
    const partialArtifacts=spawnSync('bash',[script],options);
    assert.equal(partialArtifacts.status,1);
    assert.equal(partialArtifacts.stdout,'');
    assert.match(partialArtifacts.stderr,/census_unmeasured/);
    fs.writeFileSync(record,JSON.stringify({runs:runs.map(row=>({...row,head_branch:undefined})),available}));
    const unmeasuredBranch=spawnSync('bash',[script],options);
    assert.equal(unmeasuredBranch.status,1);
    assert.equal(unmeasuredBranch.stdout,'');
    assert.match(unmeasuredBranch.stderr,/source_census_unmeasured/);
  }finally{fs.rmSync(scratch,{recursive:true,force:true});}
});

test('source reuse binds a complete artifact census to one settled producer attempt',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-source-reuse-attempt-'));
  try{
    const calls=path.join(scratch,'calls');
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
import fs from 'node:fs';
const endpoint=new URL('https://api.github.com/'+process.argv[3]);
const previous=fs.existsSync(process.env.CALLS)?fs.readFileSync(process.env.CALLS,'utf8'):'';
fs.appendFileSync(process.env.CALLS,endpoint.pathname+endpoint.search+'\\n');
const source='${source}';
const listed={id:42,run_attempt:1,event:process.env.SCENARIO==='scheduled'?'schedule':'push',
  head_branch:'main',head_sha:source,status:'completed',conclusion:'success',
  path:'.github/workflows/build-release-binaries.yml',repository:{full_name:'burin-labs/harn'},
  head_repository:{full_name:'burin-labs/harn'}};
if(endpoint.pathname.includes('/workflows/')) console.log(JSON.stringify({total_count:1,workflow_runs:[listed]}));
else if(endpoint.pathname.endsWith('/artifacts')) {
  const names=['harn-a','candidate-manifest-'+source,'harn-release-files'];
  const artifacts=names.map((name,index)=>({id:100+index,name,expired:false,size_in_bytes:100,
    digest:'sha256:'+'d'.repeat(64),workflow_run:{id:42,head_sha:source}}));
  artifacts[1].digest=process.env.MANIFEST_DIGEST;
  artifacts[1].size_in_bytes=Number(process.env.MANIFEST_SIZE);
  if(process.env.SCENARIO==='duplicate') artifacts.push({...artifacts[0],id:103});
  if(process.env.SCENARIO==='foreign') artifacts[0].workflow_run.id=99;
  if(process.env.SCENARIO==='changed_artifact'&&previous.includes('/runs/42/artifacts?')) artifacts[0].id=104;
  console.log(JSON.stringify({total_count:artifacts.length+(process.env.SCENARIO==='partial'?1:0),artifacts}));
} else if(endpoint.pathname.endsWith('/artifacts/101/zip')) {
  process.stdout.write(fs.readFileSync(process.env.MANIFEST_ZIP));
} else if(endpoint.pathname.endsWith('/attempts/1/jobs')) {
  const jobs=['Build a','Assemble candidate manifest'].map((name,index)=>({id:500+index,name,run_id:42,
    run_attempt:1,head_sha:source,status:'completed',conclusion:'success'}));
  if(process.env.SCENARIO==='missing_job') jobs.pop();
  if(process.env.SCENARIO==='duplicate_job') jobs.push({...jobs[0],id:503,conclusion:'failure'});
  console.log(JSON.stringify({total_count:jobs.length,jobs}));
} else if(endpoint.pathname.endsWith('/runs/42/attempts/1')) {
  console.log(JSON.stringify(process.env.SCENARIO==='attempt_mismatch'?{...listed,conclusion:'failure'}:listed));
} else if(endpoint.pathname.endsWith('/runs/42')) {
  const changed=process.env.SCENARIO==='early_change'||
    (process.env.SCENARIO==='late_change'&&previous.includes('/runs/42\\n'));
  console.log(JSON.stringify(changed?{...listed,run_attempt:2,status:'in_progress',conclusion:null}:listed));
} else process.exit(1);
`,{mode:0o700});
    const entry=path.join(scratch,'reuse.sh');
    fs.writeFileSync(entry,`#!/usr/bin/env bash\nset -euo pipefail\nsource '${fileURLToPath(new URL('../lib/release_candidate_run.sh',import.meta.url))}'\nsource_candidate_live_run_id burin-labs/harn '${source}' '[{"target":"a"}]'\n`);
    for(const scenario of ['good','scheduled','early_change','late_change','duplicate','foreign','partial','missing_job',
      'manifest_attempt','manifest_source','attempt_mismatch','extra_archive','duplicate_job','changed_artifact']){
      fs.writeFileSync(calls,'');
      const manifest={schemaVersion:'burin-labs.candidate_manifest.v1',repository,sourceCommit:source,
        runId:'42',runAttempt:scenario==='manifest_attempt'?'2':'1',artifacts:[
          {kind:'archive',target:'a',artifact:'harn-a',sha256:'d'.repeat(64)},
          {kind:'notes',artifact:'harn-release-files',sha256:'e'.repeat(64)}]};
      if(scenario==='manifest_source')manifest.sourceCommit='f'.repeat(40);
      if(scenario==='extra_archive')manifest.artifacts.push({kind:'archive',target:'a',artifact:'other',sha256:'d'.repeat(64)});
      const archive=path.join(scratch,'manifest.zip');
      fs.writeFileSync(path.join(scratch,'candidate-manifest.json'),JSON.stringify(manifest));
      fs.rmSync(archive,{force:true});
      execFileSync('zip',['-q','-X',archive,'candidate-manifest.json'],{cwd:scratch});
      const bytes=fs.readFileSync(archive);
      const result=spawnSync('bash',[entry],{encoding:'utf8',env:{...process.env,PATH:`${scratch}:${process.env.PATH}`,
        CALLS:calls,SCENARIO:scenario,GH_TOKEN:'fixture-read',MANIFEST_ZIP:archive,
        MANIFEST_SIZE:String(bytes.length),MANIFEST_DIGEST:`sha256:${createHash('sha256').update(bytes).digest('hex')}`}});
      if(scenario==='good'||scenario==='scheduled'){
        assert.equal(result.status,0,result.stderr);
        assert.equal(result.stdout.trim(),'42');
      }else{
        assert.notEqual(result.status,0,`${scenario} must refuse rather than schedule a new build`);
        assert.equal(result.stdout.trim(),'');
      }
    }
  }finally{fs.rmSync(scratch,{recursive:true,force:true});}
});

test('promotion binds rehearsal and manifest to the same settled attempt',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-rehearsal-attempt-'));
  try{
    const calls=path.join(scratch,'calls');
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
import fs from 'node:fs';
const endpoint=new URL('https://api.github.com/'+process.argv[3]);
const previous=fs.existsSync(process.env.CALLS)?fs.readFileSync(process.env.CALLS,'utf8'):'';
fs.appendFileSync(process.env.CALLS,endpoint.pathname+endpoint.search+'\\n');
const source='${source}',head='${head}';
const run={id:42,run_attempt:1,head_sha:source,status:'completed',conclusion:'success',event:'push',
  path:'.github/workflows/build-release-binaries.yml',repository:{full_name:'burin-labs/harn'},
  head_repository:{full_name:'burin-labs/harn'}};
if(endpoint.pathname.includes('/acme/widget-host')){
  if(endpoint.pathname.endsWith('/actions/runs/77'))console.log(head);
  else if(endpoint.pathname.includes('/compare/'))console.log('identical 0');
  else console.log('main');
}else if(endpoint.pathname.endsWith('/runs/42/attempts/1'))console.log(JSON.stringify(run));
else if(endpoint.pathname.endsWith('/runs/42/attempts/1/jobs')){
  const job={id:600,name:'Consumer release rehearsal / Consumer canary',run_id:42,run_attempt:1,
    head_sha:source,status:'completed',conclusion:process.env.SCENARIO==='job_failure'?'failure':'success'};
  console.log(JSON.stringify({total_count:1,jobs:[job]}));
}
else if(endpoint.pathname.endsWith('/runs/42')){
  const n=previous.split('/runs/42\\n').length-1;
  const changed=(process.env.SCENARIO==='changed'&&n>=1)||
    (process.env.SCENARIO==='changed_after_consumer'&&n>=2);
  console.log(JSON.stringify(changed?{...run,run_attempt:2,status:'in_progress',conclusion:null}:run));
}else if(endpoint.pathname.endsWith('/runs/42/artifacts')){
  const artifacts=['candidate-manifest-'+source,'consumer-rehearsal-binding-1'].map((name,index)=>({
    id:100+index,name,expired:false,size_in_bytes:Number(index?process.env.BINDING_SIZE:process.env.MANIFEST_SIZE),
    digest:index?process.env.BINDING_DIGEST:process.env.MANIFEST_DIGEST,workflow_run:{id:42,head_sha:source}}));
  if(process.env.SCENARIO==='duplicate')artifacts.push({...artifacts[1],id:102});
  if(process.env.SCENARIO==='wrong_name')artifacts[1].name='consumer-rehearsal-binding-2';
  if(process.env.SCENARIO==='bad_digest')artifacts[0].digest='sha256:'+'a'.repeat(64);
  console.log(JSON.stringify({total_count:artifacts.length+(process.env.SCENARIO==='partial'?1:0),artifacts}));
}else if(endpoint.pathname.endsWith('/artifacts/100/zip'))process.stdout.write(fs.readFileSync(process.env.MANIFEST_ZIP));
else if(endpoint.pathname.endsWith('/artifacts/101/zip'))process.stdout.write(fs.readFileSync(process.env.BINDING_ZIP));
else process.exit(1);
`,{mode:0o700});
    const script=path.join(scratch,'binding.sh');
    fs.writeFileSync(script,`#!/usr/bin/env bash\nset -euo pipefail\nsource '${fileURLToPath(new URL('../lib/release_consumer_verdict.sh',import.meta.url))}'\nrelease_require_rehearsal_binding burin-labs/harn 42 '${source}'\n`);
    for(const scenario of ['good','partial','duplicate','wrong_name','wrong_attempt','manifest_attempt',
      'wrong_manifest','changed','changed_after_consumer','job_failure','bad_digest']){
      fs.writeFileSync(calls,'');
      const manifest={schemaVersion:'burin-labs.candidate_manifest.v1',repository,sourceCommit:source,
        runId:'42',runAttempt:scenario==='manifest_attempt'?'2':'1',artifacts:[]};
      const manifestFile=path.join(scratch,'candidate-manifest.json'),manifestZip=path.join(scratch,'manifest.zip');
      fs.writeFileSync(manifestFile,JSON.stringify(manifest));fs.rmSync(manifestZip,{force:true});
      execFileSync('zip',['-q','-X',manifestZip,'candidate-manifest.json'],{cwd:scratch});
      const manifestBytes=fs.readFileSync(manifestZip),manifestDigest=`sha256:${createHash('sha256').update(manifestBytes).digest('hex')}`;
      const binding={schema:'harn.release.consumer_rehearsal_binding.v1',verdict:'pass',source_sha:source,
        producer_run:'42',producer_attempt:scenario==='wrong_attempt'?2:1,
        manifest_digest:scenario==='wrong_manifest'?`sha256:${'f'.repeat(64)}`:manifestDigest,
        consumer_run:'77',consumer_head_sha256:createHash('sha256').update(head).digest('hex'),consumer_pull:null};
      const bindingFile=path.join(scratch,'consumer-rehearsal-binding.json'),bindingZip=path.join(scratch,'binding.zip');
      fs.writeFileSync(bindingFile,JSON.stringify(binding));fs.rmSync(bindingZip,{force:true});
      execFileSync('zip',['-q','-X',bindingZip,'consumer-rehearsal-binding.json'],{cwd:scratch});
      const bindingBytes=fs.readFileSync(bindingZip);
      const result=spawnSync('bash',[script],{encoding:'utf8',env:{...process.env,PATH:`${scratch}:${process.env.PATH}`,
        CALLS:calls,SCENARIO:scenario,GH_TOKEN:'fixture-read',CANARY_REPOSITORY:'acme/widget-host',
        CONSUMER_READ_TOKEN:'fixture-consumer',MANIFEST_ZIP:manifestZip,MANIFEST_SIZE:String(manifestBytes.length),
        MANIFEST_DIGEST:manifestDigest,BINDING_ZIP:bindingZip,BINDING_SIZE:String(bindingBytes.length),
        BINDING_DIGEST:`sha256:${createHash('sha256').update(bindingBytes).digest('hex')}`}});
      if(scenario==='good')assert.equal(result.status,0,result.stderr);
      else assert.notEqual(result.status,0,`${scenario} accepted: ${result.stderr}`);
    }
  }finally{fs.rmSync(scratch,{recursive:true,force:true});}
});
