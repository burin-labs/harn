import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync,spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {activePromotions,successfulCandidate,ghRead,rehearsalBinding,publicationRehearsalBinding} from '../lib/release-candidate-state.mjs';
import {archiveJson} from './fixtures/artifact_archive.mjs';
import {compositionFixture,repository as compositionRepository,head as compositionHead,
  declaration as compositionDeclaration} from './fixtures/consumer_composition.mjs';
import {admitChild,archiveProducerBinding,qualifyArchiveProducer,observerRunName,
  observerDeclarationName,observerDeclarationStep,observerPublicationWitness,
  observerDispatchActor,githubConsumerSelection} from '../lib/consumer-observer.mjs';

const source='a'.repeat(40),policy='b'.repeat(40),head='c'.repeat(40);

test('release pairing reads the complete exact source range without ambient overrides', () => {
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-pairing-source-'));
  try {
    const git=(...args)=>execFileSync('git',['-C',scratch,...args],{encoding:'utf8',
      env:{...process.env,GIT_CONFIG_GLOBAL:'/dev/null',GIT_CONFIG_SYSTEM:'/dev/null'}}).trim();
    git('init','-q');
    git('config','user.name','Fixture');
    git('config','user.email','fixture@example.invalid');
    const commit=message=>{git('-c','commit.gpgsign=false','commit','-q','--allow-empty','-m',message);return git('rev-parse','HEAD');};
    fs.writeFileSync(path.join(scratch,'Cargo.toml'),'[workspace.package]\nversion = "0.10.1"\n');
    git('add','Cargo.toml');
    commit('Prior release\n\nPairs-with: consumer#1');
    git('tag','v0.10.0');
    commit('First adaptation\n\nPairs-with: consumer#7');
    git('tag','v0.10.1-rc.1');
    const selected=commit('Second adaptation\n\nPairs-with: consumer#8\nConsumer-composition: consumer#9');
    commit('Later unrelated work\n\nPairs-with: consumer#10');
    const script=path.join(scratch,'read.sh');
    fs.writeFileSync(script,'set -euo pipefail\nsource "$1"\nconsumer_release_pairing_source "$2" "$3" "$4"\nprintf "%s\\0%s\\0%s" "$CONSUMER_PAIRING_BASE" "$CONSUMER_PAIRING_COUNT" "$CONSUMER_PAIRING_TEXT"\n');
    const helper=fileURLToPath(new URL('../lib/consumer_pairing_source.sh',import.meta.url));
    const read=(revision=selected,version='0.10.1',directory=scratch)=>spawnSync('bash',[script,helper,directory,revision,version],
      {encoding:'utf8',env:{...process.env,PAIRING_TEXT:'Pairs-with: consumer#99',CANARY_PAIRING_SOURCE:'/missing'}});
    const result=read();
    assert.equal(result.status,0,result.stderr);
    const [base,count,text]=result.stdout.split('\0');
    assert.equal(base,'v0.10.0');
    assert.equal(count,'2');
    assert.match(text,/Pairs-with: consumer#7/);
    assert.match(text,/Pairs-with: consumer#8/);
    assert.match(text,/Consumer-composition: consumer#9/);
    assert.doesNotMatch(text,/consumer#(?:1|10|99)(?:\D|$)/);
    assert.equal(result.stderr,'');
    const cli=spawnSync('bash',[helper,'--release-source',scratch,selected],{encoding:'utf8',
      env:{...process.env,PAIRING_TEXT:'Pairs-with: consumer#99',WORKSPACE_VERSION:'invalid'}});
    assert.equal(cli.status,0,cli.stderr);
    assert.equal(cli.stdout,text);
    assert.equal(cli.stderr,'');
    for(const [revision,version,directory] of [['bad','0.10.1',scratch],['f'.repeat(40),'0.10.1',scratch],
      [selected,'0.10.1-rc.1',scratch],[selected,'0.10.1','/missing']]) {
      const refused=read(revision,version,directory);
      assert.notEqual(refused.status,0);
      assert.equal(refused.stdout,'');
      assert.match(refused.stderr,/reason=pairing_range_unmeasured/);
    }
    fs.writeFileSync(path.join(scratch,'Cargo.toml'),'[workspace.package]\nversion = "invalid"\n');
    git('add','Cargo.toml');
    const invalidVersion=commit('Invalid workspace version');
    const refusedCli=spawnSync('bash',[helper,'--release-source',scratch,invalidVersion],{encoding:'utf8'});
    assert.notEqual(refusedCli.status,0);
    assert.equal(refusedCli.stdout,'');
    assert.match(refusedCli.stderr,/reason=pairing_range_unmeasured/);
    git('tag','-d','v0.10.0');
    const tagless=read();
    assert.notEqual(tagless.status,0);
    assert.equal(tagless.stdout,'');
    assert.match(tagless.stderr,/reason=pairing_range_unmeasured/);
  } finally {fs.rmSync(scratch,{recursive:true,force:true});}
});

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
  .map((name,index)=>({id:index+1,name,run_id:42,run_attempt:1,head_sha:source,status:'completed',conclusion:'success',
    steps:(name==='Assemble candidate manifest'?['Upload candidate manifest','Upload release files']:
      name.startsWith('Build ')?[`Upload candidate archive (${name.includes('windows')?'windows':'unix'})`]:[])
      .map(name=>({name,status:'completed',conclusion:'success',started_at:'2026-11-02T10:00:00Z',completed_at:'2026-11-02T10:00:02Z'}))}));
const artifacts=[...targets.map(target=>`harn-${target}`),`candidate-manifest-${source}`,'harn-release-files']
  .map((name,index)=>({id:index+100,name,created_at:'2026-11-02T10:00:01Z',expired:false,size_in_bytes:100,digest:`sha256:${'d'.repeat(64)}`,
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
    const archived=archiveJson(scratch,filename,value,
      {id,name,created_at:'2026-11-02T10:00:01Z',workflow_run:{id:runId,head_sha:sha}});
    return {bytes:fs.readFileSync(archived.archive),artifact:archived.metadata};
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
    fs.writeFileSync(path.join(scratch,'gh'),'#!/usr/bin/env node\nif(!process.argv.includes("--include"))process.exit(2);\nprocess.stdout.write("HTTP/2.0 200 OK\\r\\nContent-Type: application/zip\\r\\n\\r\\n");\nprocess.stdout.write(Buffer.from([0,255,128,65]));\n',{mode:0o700});
    process.env.PATH=`${scratch}:${previous}`;
    const response=await ghRead('https://api.github.com/repos/acme/test/actions/artifacts/1/zip');
    assert.deepEqual(Buffer.from(await response.arrayBuffer()),Buffer.from([0,255,128,65]));
    await assert.rejects(ghRead('https://api.github.com/repos/acme/test',{method:'POST'}),/read_endpoint_invalid/);
  }finally{process.env.PATH=previous;fs.rmSync(scratch,{recursive:true,force:true});}
});
test('publication gh bridge preserves a complete 101-commit composition and refuses partial pages',async()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-gh-composition-pages-'));
  const previous=process.env.PATH;
  try{
    const fixture=compositionFixture();
    const base='a'.repeat(40),comparison=`/compare/${base}...${compositionHead}?per_page=100&page=`;
    const original=fixture.records.get(`${comparison}1`);
    const history=Array.from({length:94},(_,index)=>({sha:index.toString(16).padStart(40,'0'),parents:[{sha:base}]}));
    const all=[...history,...original.commits];
    assert.equal(all.length,101);
    fixture.records.set(`${comparison}1`,{...original,total_commits:101,commits:all.slice(0,100)});
    fixture.records.set(`${comparison}2`,{...original,total_commits:101,commits:all.slice(100)});
    const component=`/compare/${base}...${'b'.repeat(40)}?per_page=100&page=1`;
    fixture.records.set(component,{...fixture.records.get(component),total_commits:95,
      commits:[...history,...fixture.records.get(component).commits]});
    const records=path.join(scratch,'records.json'),calls=path.join(scratch,'calls.jsonl');
    const mode=path.join(scratch,'mode');
    fs.writeFileSync(records,JSON.stringify([...fixture.records]));
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
const fs=require('node:fs');
const args=process.argv.slice(2),mode=fs.readFileSync(${JSON.stringify(mode)},'utf8');
if(args[0]!=='api'||!args.includes('--include')||process.env.GH_TOKEN!=='consumer-read'||process.env.GITHUB_TOKEN)process.exit(2);
const suffix=args[1].slice('repos/${compositionRepository}'.length);
fs.appendFileSync(${JSON.stringify(calls)},JSON.stringify({suffix,token:process.env.GH_TOKEN})+'\\n');
const records=new Map(JSON.parse(fs.readFileSync(${JSON.stringify(records)},'utf8')));
if(!records.has(suffix))process.exit(3);
if(mode==='refused'&&suffix===${JSON.stringify(`${comparison}2`)}){
  process.stdout.write('HTTP/2.0 403 Forbidden\\r\\nContent-Type: application/json\\r\\n\\r\\n{}');process.exit(1);
}
process.stdout.write('HTTP/2.0 200 OK\\r\\nContent-Type: application/json\\r\\n');
if(mode!=='missing-link'&&suffix===${JSON.stringify(`${comparison}1`)})process.stdout.write('Link: <https://api.github.com/next>; rel="next"\\r\\n');
process.stdout.write('\\r\\n');
const value=records.get(suffix);
if(mode==='partial'&&suffix===${JSON.stringify(`${comparison}2`)})value.commits=[];
process.stdout.write(JSON.stringify(value));
`,{mode:0o700});
    process.env.PATH=`${scratch}:${previous}`;
    for(const scenario of ['good','missing-link','partial','refused']){
      fs.writeFileSync(mode,scenario);fs.writeFileSync(calls,'');
      const result=githubConsumerSelection('consumer-read',compositionRepository,compositionDeclaration(),ghRead);
      if(scenario==='good')assert.equal((await result).headSha,compositionHead);
      else await assert.rejects(result,/consumer_composition_graph_unmeasured|consumer_source_read_refused/);
      const observed=fs.readFileSync(calls,'utf8').trim().split('\n').map(line=>JSON.parse(line));
      assert.ok(observed.some(call=>call.suffix===`${comparison}1`));
      assert.equal(observed.some(call=>call.suffix===`${comparison}2`),scenario!=='missing-link');
      assert.ok(observed.every(call=>call.token==='consumer-read'));
    }
  }finally{process.env.PATH=previous;fs.rmSync(scratch,{recursive:true,force:true});}
});
test('gh bridge measures HTTP status and headers and refuses malformed or failed envelopes',async()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-gh-status-'));
  const previous=process.env.PATH;
  try{
    const responseFile=path.join(scratch,'response'),codeFile=path.join(scratch,'exit');
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
const fs=require('node:fs');
process.stdout.write(fs.readFileSync(${JSON.stringify(responseFile)}));
process.exit(Number(fs.readFileSync(${JSON.stringify(codeFile)},'utf8')));
`,{mode:0o700});
    process.env.PATH=`${scratch}:${previous}`;
    const read=(bytes,code=0)=>{
      fs.writeFileSync(responseFile,bytes);fs.writeFileSync(codeFile,String(code));
      return ghRead('https://api.github.com/repos/fixture/consumer');
    };
    const refused=await read('HTTP/2.0 404 Not Found\r\nContent-Type: application/json\r\nX-RateLimit-Remaining: 19\r\n\r\n{"message":"Not Found"}',1);
    assert.equal(refused.status,404);assert.equal(refused.ok,false);
    assert.equal(refused.headers.get('x-ratelimit-remaining'),'19');
    assert.deepEqual(await refused.json(),{message:'Not Found'});
    assert.equal((await read('HTTP/2.0 204 No Content\n\n')).status,204);
    for(const [bytes,code] of [['{}',0],['HTTP/2.0 nope\r\n\r\n{}',0],
      ['HTTP/2.0 200 OK\r\nNot a header\r\n\r\n{}',0],['HTTP/2.0 200 OK\r\n\r\n{}',1],
      ['HTTP/2.0 204 No Content\r\n\r\n{}',0]]){
      await assert.rejects(read(bytes,code),/github_read_unmeasured/);
    }
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
    const entry=available.find(item=>item.name===`candidate-manifest-${source}`);
    const {archive,metadata:measured}=archiveJson(scratch,'candidate-manifest.json',manifest,entry);
    const bytes=fs.readFileSync(archive);
    Object.assign(entry,measured);
    fs.writeFileSync(record,JSON.stringify({runs,available,partial:false}));
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
import fs from 'node:fs';
const endpoint=new URL('https://api.github.com/'+process.argv[3]);
if(process.argv.includes('--include'))process.stdout.write('HTTP/2.0 200 OK\\r\\nContent-Type: application/json\\r\\n\\r\\n');
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
if(process.argv.includes('--include'))process.stdout.write('HTTP/2.0 200 OK\\r\\nContent-Type: application/json\\r\\n\\r\\n');
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
      if(['good','early_change','late_change','partial','manifest_attempt','manifest_source',
        'attempt_mismatch','changed_artifact'].includes(scenario)){
        fs.writeFileSync(calls,'');
        const reader=fileURLToPath(new URL('../lib/release-candidate-state.mjs',import.meta.url));
        const identity=spawnSync(process.execPath,[reader,'producer-identity',repository,'42',source],{
          encoding:'utf8',env:{...process.env,PATH:`${scratch}:${process.env.PATH}`,CALLS:calls,
            SCENARIO:scenario,GH_TOKEN:'fixture-read',MANIFEST_ZIP:archive,MANIFEST_SIZE:String(bytes.length),
            MANIFEST_DIGEST:`sha256:${createHash('sha256').update(bytes).digest('hex')}`}});
        if(scenario==='good'){
          assert.equal(identity.status,0,identity.stderr);
          assert.deepEqual(JSON.parse(identity.stdout),{producer_attempt:1,manifest_id:101,
            manifest_digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`});
        }else{
          assert.notEqual(identity.status,0,`${scenario} must refuse an unproved producer identity`);
          assert.equal(identity.stdout,'');
        }
      }
    }
  }finally{fs.rmSync(scratch,{recursive:true,force:true});}
});

test('workflow identity projection rejects partial or malformed tuples',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-producer-output-'));
  try{
    const output=path.join(scratch,'outputs');
    const library=fileURLToPath(new URL('../lib/release_consumer_verdict.sh',import.meta.url));
    const script=path.join(scratch,'projection.sh');
    fs.writeFileSync(script,`#!/usr/bin/env bash\nset -euo pipefail\nsource '${library}'\nrelease_write_producer_identity "$1"\n`);
    const valid={producer_attempt:1,manifest_id:100,manifest_digest:`sha256:${'a'.repeat(64)}`};
    for(const identity of [valid,{...valid,producer_attempt:0},{...valid,manifest_id:null},
      {...valid,manifest_digest:'sha256:bad'}, {producer_attempt:1,manifest_id:100}]){
      fs.writeFileSync(output,'');
      const result=spawnSync('bash',[script,JSON.stringify(identity)],{encoding:'utf8',env:{...process.env,GITHUB_OUTPUT:output}});
      if(identity===valid){
        assert.equal(result.status,0,result.stderr);
        assert.equal(fs.readFileSync(output,'utf8'),`expected_attempt=1\nexpected_manifest_id=100\nexpected_manifest_digest=${valid.manifest_digest}\n`);
      }else{
        assert.notEqual(result.status,0);
        assert.equal(fs.readFileSync(output,'utf8'),'');
      }
    }
  }finally{fs.rmSync(scratch,{recursive:true,force:true});}
});

test('promotion binds rehearsal and manifest to the same settled attempt',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-rehearsal-attempt-'));
  try{
    const calls=path.join(scratch,'calls');
    fs.writeFileSync(path.join(scratch,'git'),`#!/usr/bin/env node
import fs from 'node:fs';
const args=process.argv.slice(2);
if(args[0]==='-C')args.splice(0,2);
const source='${source}';
if(process.env.GIT_CALLS)fs.appendFileSync(process.env.GIT_CALLS,'git:'+args.join(' ')+'\\n');
if(args[0]==='fetch'&&args.slice(1).join(' ')==='--no-tags origin '+source)process.exit(0);
else if(args[0]==='show'&&args[1]===source+':Cargo.toml')console.log('[workspace.package]\\nversion = "0.10.200"');
else if(args[0]==='rev-parse'&&args[2]===source+'^{commit}')console.log(source);
else if(args[0]==='tag'&&args[1]==='--list')console.log('v0.10.199');
else if(args[0]==='rev-list'&&args[1]==='--count')console.log('1');
else if(args[0]==='log'&&args[1]==='--format=%B')console.log(process.env.SCENARIO.startsWith('composition_')?
  'Pairs-with: consumer#7\\nPairs-with: consumer#8\\nConsumer-composition: consumer#9':'Ordinary release');
else process.exit(1);
`,{mode:0o700});
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
import fs from 'node:fs';
const endpoint=new URL('https://api.github.com/'+process.argv[3]);
if(process.argv.includes('--include'))process.stdout.write('HTTP/2.0 200 OK\\r\\nContent-Type: application/json\\r\\n\\r\\n');
const previous=fs.existsSync(process.env.CALLS)?fs.readFileSync(process.env.CALLS,'utf8'):'';
fs.appendFileSync(process.env.CALLS,endpoint.pathname+endpoint.search+'\\n');
const source='${source}',head='${head}';
const run={id:42,run_attempt:1,head_sha:source,status:'completed',conclusion:'success',event:'push',
  path:'.github/workflows/build-release-binaries.yml',repository:{full_name:'burin-labs/harn'},
  head_repository:{full_name:'burin-labs/harn'}};
if(endpoint.pathname.startsWith('/repos/'+process.env.CONSUMER_NAME)){
  const suffix=endpoint.pathname.replace('/repos/'+process.env.CONSUMER_NAME,'')+endpoint.search;
  const records=new Map(JSON.parse(fs.readFileSync(process.env.CONSUMER_RECORDS,'utf8')));
  if(process.argv.includes('--jq')){
    if(endpoint.pathname.endsWith('/actions/runs/77'))console.log(process.env.SCENARIO==='composition_child_moved'?'4'.repeat(40):process.env.CONSUMER_HEAD);
    else if(endpoint.pathname.includes('/pulls/')){
      const pull=records.get(suffix);
      console.log(pull.state+' '+pull.merged+' '+pull.head.repo.full_name+' '+pull.head.sha);
    }else if(endpoint.pathname.includes('/compare/'))console.log('identical 0');
    else console.log('main');
  }else{
    if(process.env.GH_TOKEN!=='fixture-consumer')process.exit(2);
    const record=records.get(suffix);
    if(!record){console.log('null');process.exit(0);}
    const seen=previous.split(endpoint.pathname+endpoint.search+'\\n').length-1;
    if(process.env.SCENARIO==='composition_late_component'&&suffix==='/pulls/7'&&seen>=2)record.head.sha='4'.repeat(40);
    console.log(JSON.stringify(record));
  }
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
      'wrong_manifest','changed','changed_after_consumer','job_failure','bad_digest','composition_v1',
      'composition_good','composition_missing_component','composition_child_moved','composition_late_component','composition_wrong_manifest']){
      fs.writeFileSync(calls,'');
      const manifest={schemaVersion:'burin-labs.candidate_manifest.v1',repository,sourceCommit:source,
        runId:'42',runAttempt:scenario==='manifest_attempt'?'2':'1',artifacts:[]};
      const manifestFile=path.join(scratch,'candidate-manifest.json'),manifestZip=path.join(scratch,'manifest.zip');
      fs.writeFileSync(manifestFile,JSON.stringify(manifest));fs.rmSync(manifestZip,{force:true});
      execFileSync('zip',['-q','-X',manifestZip,'candidate-manifest.json'],{cwd:scratch});
      const manifestBytes=fs.readFileSync(manifestZip),manifestDigest=`sha256:${createHash('sha256').update(manifestBytes).digest('hex')}`;
      const composed=scenario.startsWith('composition_')&&scenario!=='composition_v1';
      const composition=composed?compositionFixture():null;
      const consumerName=composed?compositionRepository:'acme/widget-host';
      const consumerHead=composed?compositionHead:head;
      if(scenario==='composition_missing_component')composition.records.delete('/pulls/8');
      const consumerRecords=path.join(scratch,'consumer-records.json');
      fs.writeFileSync(consumerRecords,JSON.stringify(composition?[...composition.records]:[]));
      const binding={schema:`harn.release.consumer_rehearsal_binding.v${composed?'2':'1'}`,verdict:'pass',source_sha:source,
        producer_run:'42',producer_attempt:scenario==='wrong_attempt'?2:1,
        manifest_digest:scenario==='wrong_manifest'?`sha256:${'f'.repeat(64)}`:manifestDigest,
        consumer_run:'77',consumer_head_sha256:createHash('sha256').update(consumerHead).digest('hex'),consumer_pull:composed?'9':null,
        ...(composed?{declared_pull:'9',composition:{schema:'harn.consumer_composition_identity.v1',pull:'9',requiredPulls:['7','8'],
          manifestSha256:scenario==='composition_wrong_manifest'?'f'.repeat(64):
            createHash('sha256').update('harn.consumer_composition_manifest.v1\0').update(composition.bytes).digest('hex')}}:{})};
      const bindingFile=path.join(scratch,'consumer-rehearsal-binding.json'),bindingZip=path.join(scratch,'binding.zip');
      fs.writeFileSync(bindingFile,JSON.stringify(binding));fs.rmSync(bindingZip,{force:true});
      execFileSync('zip',['-q','-X',bindingZip,'consumer-rehearsal-binding.json'],{cwd:scratch});
      const bindingBytes=fs.readFileSync(bindingZip);
      const env={...process.env,PATH:`${scratch}:${process.env.PATH}`,
        CALLS:calls,SCENARIO:scenario,GH_TOKEN:'fixture-read',CANARY_REPOSITORY:consumerName,
        CONSUMER_NAME:consumerName,CONSUMER_HEAD:consumerHead,CONSUMER_RECORDS:consumerRecords,
        CONSUMER_READ_TOKEN:'fixture-consumer',MANIFEST_ZIP:manifestZip,MANIFEST_SIZE:String(manifestBytes.length),
        MANIFEST_DIGEST:manifestDigest,BINDING_ZIP:bindingZip,BINDING_SIZE:String(bindingBytes.length),
        BINDING_DIGEST:`sha256:${createHash('sha256').update(bindingBytes).digest('hex')}`};
      const result=spawnSync('bash',[script],{encoding:'utf8',env});
      if(scenario==='good'||scenario==='composition_good'){
        assert.equal(result.status,0,result.stderr);
        assert.deepEqual(JSON.parse(result.stdout),{producer_attempt:1,manifest_id:100,manifest_digest:manifestDigest});
        if(composed){
          const observed=fs.readFileSync(calls,'utf8');
          assert.ok(observed.includes('/pulls/8\n'));
          assert.ok(observed.includes('/contents/.github/harn-consumer-composition.json?ref='));
          assert.doesNotMatch(result.stdout+result.stderr,/private-consumer|combined-private-branch|fixture-consumer/);
        }
        const root=fileURLToPath(new URL('../..',import.meta.url));
        const workflow=fs.readFileSync(path.join(root,'.github/workflows/promote-release.yml'),'utf8');
        const match=/name: Re-read the producer rehearsal binding[\s\S]*?\n        run: \|\n((?:          .*\n)+)/.exec(workflow);
        assert.ok(match,'the owning workflow step must be reached');
        const step=path.join(scratch,'step.sh'),output=path.join(scratch,'step.outputs');
        fs.writeFileSync(step,match[1].replace(/^          /gm,''));fs.writeFileSync(output,'');fs.writeFileSync(calls,'');
        const stepResult=spawnSync('bash',[step],{encoding:'utf8',cwd:root,env:{...env,GITHUB_REPOSITORY:repository,GITHUB_WORKSPACE:root,
          SOURCE_SHA:source,CANDIDATE_RUN_ID:'42',GITHUB_OUTPUT:output}});
        assert.equal(stepResult.status,0,stepResult.stderr);
        assert.equal(fs.readFileSync(output,'utf8'),`expected_attempt=1\nexpected_manifest_id=100\nexpected_manifest_digest=${manifestDigest}\n`);
        if(composed){
          const scheduler=path.join(scratch,'scheduler.mjs');
          fs.writeFileSync(scheduler,`import {releaseApi} from '${new URL('../release-on-demand.mjs',import.meta.url).href}';
            const binding=await releaseApi('${repository}').qualify('42','${source}');
            console.log(JSON.stringify({schema:binding.schema,pull:binding.composition.pull}));`);
          fs.writeFileSync(calls,'');
          const scheduled=spawnSync(process.execPath,[scheduler],{encoding:'utf8',cwd:root,
            env:{...env,CONSUMER_READ_TOKEN:'',GIT_CALLS:calls,PAIRING_TEXT:'Pairs-with: consumer#99'}});
          assert.equal(scheduled.status,0,scheduled.stderr);
          assert.deepEqual(JSON.parse(scheduled.stdout),{schema:'harn.release.consumer_rehearsal_binding.v2',pull:'9'});
          const scheduledReads=fs.readFileSync(calls,'utf8');
          assert.ok(scheduledReads.includes(`git:fetch --no-tags origin ${source}\n`));
          assert.ok(scheduledReads.includes(`git:show ${source}:Cargo.toml\n`));
          assert.equal(scheduledReads.includes(`/repos/${compositionRepository}`),false);
        }
      }else{
        assert.notEqual(result.status,0,`${scenario} accepted: ${result.stderr}`);
        assert.equal(result.stdout,'');
      }
    }
  }finally{fs.rmSync(scratch,{recursive:true,force:true});}
});

function composedRehearsal(t,change=()=>{}){
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-composition-binding-'));
  t.after(()=>fs.rmSync(scratch,{recursive:true,force:true}));
  const consumer=compositionFixture();
  const identity={schema:'harn.consumer_composition_identity.v1',pull:'9',requiredPulls:['7','8'],
    manifestSha256:createHash('sha256').update('harn.consumer_composition_manifest.v1\0').update(consumer.bytes).digest('hex')};
  const manifest=archiveJson(scratch,'candidate-manifest.json',{schemaVersion:'burin-labs.candidate_manifest.v1',
    repository,sourceCommit:source,runId:'42',runAttempt:'1',artifacts:[]},
  {id:100,name:`candidate-manifest-${source}`,workflow_run:{id:42,head_sha:source}});
  const value={schema:'harn.release.consumer_rehearsal_binding.v2',verdict:'pass',source_sha:source,
    producer_run:'42',producer_attempt:1,manifest_digest:manifest.metadata.digest,
    consumer_run:'77',consumer_head_sha256:createHash('sha256').update(compositionHead).digest('hex'),
    consumer_pull:'9',declared_pull:'9',composition:identity};
  change(value,consumer);
  const binding=archiveJson(scratch,'consumer-rehearsal-binding.json',value,
    {id:101,name:'consumer-rehearsal-binding-1',workflow_run:{id:42,head_sha:source}});
  const run={...producer,status:'completed',conclusion:'success'};
  const parentCalls=[];
  const fetchRecord=async(url,options)=>{
    const endpoint=new URL(url);
    if(endpoint.pathname.startsWith(`/repos/${compositionRepository}`))return consumer.fetchRecord(url,options);
    assert.equal(options.headers.Authorization,'Bearer parent-read');
    parentCalls.push(endpoint.pathname);
    let result;
    if(endpoint.pathname.endsWith('/artifacts/100/zip'))return new Response(fs.readFileSync(manifest.archive));
    if(endpoint.pathname.endsWith('/artifacts/101/zip'))return new Response(fs.readFileSync(binding.archive));
    if(endpoint.pathname.endsWith('/runs/42/artifacts'))result={total_count:2,artifacts:[manifest.metadata,binding.metadata]};
    else if(endpoint.pathname.endsWith('/attempts/1/jobs'))result={total_count:1,jobs:[{id:600,
      name:'Consumer release rehearsal / Consumer canary',run_id:42,run_attempt:1,head_sha:source,
      status:'completed',conclusion:'success'}]};
    else if(endpoint.pathname.endsWith('/runs/42')||endpoint.pathname.endsWith('/runs/42/attempts/1'))result=run;
    else assert.fail(`unexpected parent endpoint ${endpoint.pathname}`);
    return new Response(JSON.stringify(result));
  };
  const options={declaration:compositionDeclaration()};
  const authority={token:'consumer-read',repository:compositionRepository};
  return {consumer,value,parentCalls,run,options,
    schedule:(overrides=options)=>rehearsalBinding('parent-read',repository,'42',source,fetchRecord,overrides),
    publish:(consumerAuthority=authority,overrides=options)=>publicationRehearsalBinding('parent-read',repository,
      '42',source,consumerAuthority,fetchRecord,overrides)};
}

test('scheduler qualifies exact v2 artifacts and source intent without claiming private eligibility',async t=>{
  const fixture=composedRehearsal(t);
  const result=await fixture.schedule();
  assert.deepEqual(result.composition,fixture.value.composition);
  assert.equal(result.evidence.producer_attempt,1);
  assert.equal(fixture.consumer.calls.length,0);
});

test('publication authenticates the same v2 graph and preserves closed public evidence',async t=>{
  const fixture=composedRehearsal(t);
  const result=await fixture.publish();
  for(const pull of [7,8,12,9])assert.equal(fixture.consumer.calls.filter(call=>call===`/pulls/${pull}`).length,2);
  assert.equal(fixture.parentCalls.at(-1),'/repos/burin-labs/harn/actions/runs/42');
  assert.deepEqual(result.composition,fixture.value.composition);
  for(const privateValue of [compositionRepository,compositionHead,'combined-private-branch','consumer-read']){
    assert.equal(JSON.stringify(result).includes(privateValue),false);
  }
});

for(const [name,change] of [
  ['missing required adaptation',value=>{value.composition.requiredPulls=['7'];}],
  ['wrong selected pull',value=>{value.consumer_pull='8';}],
  ['wrong declared pull',value=>{value.declared_pull='8';}],
  ['unmeasured consumer head',value=>{value.consumer_head_sha256='';}],
  ['private identity field',value=>{value.composition.privateHead=compositionHead;}],
  ['private binding field',value=>{value.privateRepository=compositionRepository;}],
])test(`v2 artifact qualification refuses ${name}`,async t=>{
  const fixture=composedRehearsal(t,change);
  await assert.rejects(fixture.schedule());
  assert.equal(fixture.consumer.calls.length,0);
});

test('v2 requires a measured composition declaration rather than an omitted or ordinary source',async t=>{
  const fixture=composedRehearsal(t);
  await assert.rejects(fixture.schedule({}),/rehearsal_source_declaration_unmeasured/);
  await assert.rejects(fixture.schedule({declaration:{kind:'ordinary',pull:null}}),
    /rehearsal_source_declaration_unmeasured/);
});

for(const [name,change] of [
  ['manifest digest changed',value=>{value.composition.manifestSha256='f'.repeat(64);}],
  ['rehearsed head changed',value=>{value.consumer_head_sha256='f'.repeat(64);}],
  ['component moved',(_value,consumer)=>{consumer.records.get('/pulls/7').head.sha='4'.repeat(40);}],
  ['required component unreadable',(_value,consumer)=>{consumer.records.set('/pulls/8',null);}],
])test(`publication refuses ${name} after authenticating private source`,async t=>{
  const fixture=composedRehearsal(t,change);
  await fixture.schedule();
  await assert.rejects(fixture.publish());
  assert.ok(fixture.consumer.calls.length>0);
});

test('publication rechecks the producer after graph reads and refuses missing consumer authority',async t=>{
  const fixture=composedRehearsal(t);
  await assert.rejects(fixture.publish(null),/consumer_read_authority_missing/);
  fixture.consumer.records.set('/pulls/9',count=>{
    if(count===2)fixture.run.run_attempt=2;
    return {number:9,state:'open',merged:false,base:{repo:{full_name:compositionRepository},ref:'main'},
      head:{repo:{full_name:compositionRepository},ref:'combined-private-branch',sha:compositionHead}};
  });
  await assert.rejects(fixture.publish(),/source_candidate_attempt_changed/);
});
