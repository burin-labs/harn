import assert from 'node:assert/strict';
import test from 'node:test';
import {execFileSync,spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {githubArchiveProducer,observerDeclarationName,observerDispatchActor,observerRunName,qualifyArchiveProducer} from '../lib/consumer-observer.mjs';

const targets=JSON.parse(execFileSync('bash',[fileURLToPath(new URL('../lib/candidate_archive_contract.sh',import.meta.url)),'--targets-json'],{encoding:'utf8'}));
const source='a'.repeat(40);
const request={sourceRevision:source,runId:'42',attempt:1};
const run={id:42,run_attempt:1,head_sha:source,head_branch:'main',event:'push',status:'in_progress',conclusion:null,
  repository:{full_name:'burin-labs/harn'},head_repository:{full_name:'burin-labs/harn'},path:'.github/workflows/build-release-binaries.yml'};
const names=['Resolve release context','Prepare CLI AOT payload','Assemble candidate manifest',
  'Consumer release rehearsal / Decide whether main moved',...targets.map(target=>`Build ${target}`),
  ...['linux','linux-arm64','windows','macos'].map(target=>`Release smoke / Release smoke (${target})`)];
const jobs=names.map((name,index)=>({id:index+1,run_id:42,run_attempt:1,head_sha:source,name,status:'completed',conclusion:'success'}));
jobs.push({id:99,run_id:42,run_attempt:1,head_sha:source,name:'Consumer release rehearsal / Consumer canary',status:'in_progress',conclusion:null});
jobs.push({id:98,run_id:42,run_attempt:1,head_sha:source,name:'Release residual audit',status:'completed',conclusion:'success'});
const artifacts=[...targets.map(target=>`harn-${target}`),`candidate-manifest-${source}`,'harn-release-files'].map((name,index)=>({
  id:index+100,name,expired:false,size_in_bytes:100,digest:`sha256:${'b'.repeat(64)}`,workflow_run:{id:42,head_sha:source},
}));

test('archive readiness is independent of a later consumer failure and requires every build and smoke',()=>{
  const proof=qualifyArchiveProducer(run,jobs,artifacts,request,targets);
  assert.equal(proof.proven_artifacts,7);
  assert.equal(proof.pending,0);
  assert.deepEqual(proof.failed_jobs,[]);
  const failedJobs=jobs.map(job=>job.id===99?{...job,status:'completed',conclusion:'failure'}:job);
  assert.equal(qualifyArchiveProducer({...run,status:'completed',conclusion:'failure'},failedJobs,artifacts,request,targets).verdict,'pass');
  for(const required of names) {
    for(const conclusion of ['failure','skipped',null]) {
      assert.throws(()=>qualifyArchiveProducer(run,jobs.map(job=>job.name===required?{...job,conclusion}:job),artifacts,request,targets),/builds_unproven/);
    }
    assert.throws(()=>qualifyArchiveProducer(run,jobs.filter(job=>job.name!==required),artifacts,request,targets),/builds_unproven/);
  }
  for(const changed of [{...run,status:'completed',conclusion:'cancelled'},{...run,run_attempt:2},
    {...run,head_sha:'c'.repeat(40)},{...run,head_repository:{full_name:'fork/harn'}},{...run,path:'.github/workflows/ci.yml'}]) {
    assert.throws(()=>qualifyArchiveProducer(changed,jobs,artifacts,request,targets),/identity_unproven/);
  }
  assert.throws(()=>qualifyArchiveProducer(run,[],artifacts,request,targets),/identity_unproven/);
  assert.throws(()=>qualifyArchiveProducer(run,[...jobs,jobs[0]],artifacts,request,targets),/identity_unproven/);
  for(const artifact of artifacts) {
    assert.throws(()=>qualifyArchiveProducer(run,jobs,artifacts.filter(item=>item!==artifact),request,targets),/artifacts_unproven/);
    for(const change of [{expired:true},{size_in_bytes:0},{digest:null},{workflow_run:{id:43,head_sha:source}}]) {
      assert.throws(()=>qualifyArchiveProducer(run,jobs,artifacts.map(item=>item===artifact?{...item,...change}:item),request,targets),/artifacts_unproven/);
    }
  }
});

test('only the owning live candidate can observe a pending audit; failed or absent audits never qualify',()=>{
  const pending=jobs.map(job=>job.id===98?{...job,status:'in_progress',conclusion:null}:job);
  const liveRequest={...request,allowPendingAudit:true};
  const proof=qualifyArchiveProducer(run,pending,artifacts,liveRequest,targets);
  assert.equal(proof.pending,1);
  assert.deepEqual(proof.pending_jobs,['Release residual audit']);
  assert.throws(()=>qualifyArchiveProducer(run,pending,artifacts,request,targets),/builds_unproven/);
  assert.throws(()=>qualifyArchiveProducer({...run,status:'completed',conclusion:'failure'},pending,artifacts,liveRequest,targets),/builds_unproven/);
  assert.throws(()=>qualifyArchiveProducer(run,jobs.filter(job=>job.id!==98),artifacts,liveRequest,targets),/builds_unproven/);
  for(const conclusion of ['failure','cancelled','timed_out','skipped',null]) {
    const failed=jobs.map(job=>job.id===98?{...job,status:'completed',conclusion}:job);
    assert.throws(()=>qualifyArchiveProducer(run,failed,artifacts,liveRequest,targets),/builds_unproven/);
  }
});

test('the owning reader uses the producing attempt and refuses partial authenticated censuses',async()=>{
  const calls=[];
  const read=async(url)=>{
    calls.push(new URL(url).pathname);
    const record=url.includes('/jobs')?{total_count:jobs.length,jobs}:url.includes('/artifacts')?
      {total_count:artifacts.length,artifacts}:run;
    return {ok:true,json:async()=>record};
  };
  assert.equal((await githubArchiveProducer('fixture-read',request,read)).verdict,'pass');
  assert.ok(calls.includes('/repos/burin-labs/harn/actions/runs/42/attempts/1'));
  assert.ok(calls.includes('/repos/burin-labs/harn/actions/runs/42/attempts/1/jobs'));
  await assert.rejects(githubArchiveProducer('fixture-read',request,async(url,options)=>url.includes('/jobs')?
    {ok:true,json:async()=>({total_count:jobs.length+1,jobs})}:read(url,options)),/census_unmeasured/);
});

test('historical source admission reads the exact manifest bytes under its main build policy',async()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-historical-producer-'));
  try {
    const policy='c'.repeat(40);
    const historicalRun={...run,head_sha:policy,event:'workflow_dispatch'};
    const historicalJobs=jobs.map(job=>({...job,head_sha:policy}));
    const manifest={schemaVersion:'burin-labs.candidate_manifest.v1',repository:'burin-labs/harn',
      sourceCommit:source,runId:'42',runAttempt:'1',artifacts:[]};
    fs.writeFileSync(path.join(scratch,'candidate-manifest.json'),JSON.stringify(manifest));
    execFileSync('zip',['-j',path.join(scratch,'manifest.zip'),path.join(scratch,'candidate-manifest.json')]);
    const bytes=fs.readFileSync(path.join(scratch,'manifest.zip'));
    const historicalArtifacts=artifacts.map(item=>({...item,workflow_run:{id:42,head_sha:policy},
      ...(item.name===`candidate-manifest-${source}`?{size_in_bytes:bytes.length,
        digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`}:{})}));
    const read=async(url)=>{
      if(url.endsWith('/zip')) return {ok:true,arrayBuffer:async()=>bytes};
      const record=url.includes('/compare/')?{status:'ahead',merge_base_commit:{sha:source}}:
        url.includes('/jobs')?{total_count:historicalJobs.length,jobs:historicalJobs}:
        url.includes('/artifacts')?{total_count:historicalArtifacts.length,artifacts:historicalArtifacts}:historicalRun;
      return {ok:true,json:async()=>record};
    };
    const proof=await githubArchiveProducer('fixture-read',request,read);
    assert.equal(proof.verdict,'pass');
    assert.equal(proof.source_sha,source);
    for(const change of [{sourceCommit:policy},{runId:'43'},{runAttempt:'2'},
      {repository:'foreign/harn'},{schemaVersion:'unknown'}]) {
      await assert.rejects(githubArchiveProducer('fixture-read',request,read,()=>({...manifest,...change})),/identity_unproven/);
    }
    await assert.rejects(githubArchiveProducer('fixture-read',request,async(url)=>url.includes('/compare/')?
      {ok:true,json:async()=>({status:'diverged',merge_base_commit:{sha:source}})}:read(url)),/identity_unproven/);
    await assert.rejects(githubArchiveProducer('fixture-read',request,async(url)=>url.endsWith('/zip')?
      {ok:true,arrayBuffer:async()=>Buffer.from('different artifact')}:read(url)),/digest_mismatch/);
    assert.throws(()=>qualifyArchiveProducer(historicalRun,historicalJobs,historicalArtifacts,request,targets),/identity_unproven/);
  } finally {
    fs.rmSync(scratch,{recursive:true,force:true});
  }
});

test('actual admission permits a pending audit only for its own live candidate',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-candidate-admission-'));
  try {
    const context={schema:'harn.consumer_observer.v2',parent_repository:'burin-labs/harn',parent_run_id:'42',
      parent_run_attempt:1,parent_head_sha:source,source_revision:source,started_at:1000,
      dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab'};
    const child={id:73,run_attempt:1,head_sha:'c'.repeat(40),head_repository:{full_name:'acme/widget-host'},
      path:'.github/workflows/rehearsal.yml',head_branch:'main',event:'workflow_dispatch',status:'in_progress',
      actor:{login:observerDispatchActor,type:'Bot'},display_title:observerRunName(context)};
    const declaration=path.join(scratch,'consumer-observer.json'),archive=path.join(scratch,'declaration.zip');
    fs.writeFileSync(declaration,JSON.stringify(context));
    const zipped=spawnSync('zip',['-j',archive,declaration],{encoding:'utf8'});
    assert.equal(zipped.status,0,zipped.stderr);
    const bytes=fs.readFileSync(archive);
    const declarationArtifact={id:500,name:observerDeclarationName(context),expired:false,size_in_bytes:bytes.length,
      digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,workflow_run:{id:42,head_sha:source}};
    const fixture=path.join(scratch,'fixture.json'),preload=path.join(scratch,'fetch.mjs');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const data=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
      globalThis.fetch=async(url)=>{
        const endpoint=new URL(url),route=endpoint.pathname;
        if(route.endsWith('/zip'))return {ok:true,arrayBuffer:async()=>fs.readFileSync(${JSON.stringify(archive)})};
        const record=route.endsWith('/73')?data.child:route.endsWith('/jobs')?
          {total_count:data.jobs.length,jobs:data.jobs}:route.endsWith('/artifacts')?
          (endpoint.searchParams.has('name')?{total_count:1,artifacts:[data.declaration]}:
            {total_count:data.artifacts.length,artifacts:data.artifacts}):data.run;
        return {ok:true,json:async()=>record};
      };`);
    const event=path.join(scratch,'event.json');
    fs.writeFileSync(event,JSON.stringify({inputs:{source_revision:source,observer:JSON.stringify(context),
      release_producer_run_id:'42',release_producer_run_attempt:'1'}}));
    const entry=fileURLToPath(new URL('../ci/consumer-observer-admit.mjs',import.meta.url));
    let sequence=0;
    const execute=(changes={})=>{
      const directory=path.join(scratch,String(sequence++));fs.mkdirSync(directory);
      const output=path.join(directory,'output');
      fs.writeFileSync(fixture,JSON.stringify({run,jobs,artifacts,child,declaration:declarationArtifact,...changes}));
      const result=spawnSync(process.execPath,[entry],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${preload}`,GH_TOKEN:'fixture-read',OBSERVER_CONTEXT:JSON.stringify(context),
        GITHUB_REPOSITORY:'acme/widget-host',GITHUB_RUN_ID:'73',GITHUB_RUN_ATTEMPT:'1',GITHUB_SHA:child.head_sha,
        GITHUB_WORKFLOW_REF:'acme/widget-host/.github/workflows/rehearsal.yml@refs/heads/main',
        GITHUB_EVENT_PATH:event,SOURCE_REVISION:source,RELEASE_PRODUCER_RUN_ID:'42',
        RELEASE_PRODUCER_RUN_ATTEMPT:'1',RUNNER_TEMP:directory,GITHUB_OUTPUT:output}});
      return {result,text:fs.existsSync(output)?fs.readFileSync(output,'utf8'):''};
    };
    const pending=jobs.map(job=>job.id===98?{...job,status:'in_progress',conclusion:null}:job);
    const accepted=execute({jobs:pending});
    assert.equal(accepted.result.status,0,accepted.result.stderr);
    assert.match(accepted.text,/^owned=true\n/);
    const receipt=JSON.parse(fs.readFileSync(/^receipt=(.+)$/m.exec(accepted.text)[1],'utf8'));
    assert.equal(receipt.producer.producer_run,'42');
    for(const changes of [
      {run:{...run,path:'.github/workflows/promote-release.yml'},jobs:pending},
      {jobs:jobs.filter(job=>job.id!==98)},
      ...['failure','cancelled','timed_out','skipped',null].map(conclusion=>
        ({jobs:jobs.map(job=>job.id===98?{...job,status:'completed',conclusion}:job)})),
    ]) {
      const refused=execute(changes);
      assert.equal(refused.result.status,1);
      assert.equal(refused.text,'','refusal must not publish a custody receipt');
    }
  } finally {
    fs.rmSync(scratch,{recursive:true,force:true});
  }
});

test('the actual recovery dispatcher authenticates its live promotion and preserves original archive identity',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-producer-recovery-'));
  try {
    const context={schema:'harn.consumer_observer.v2',parent_repository:'burin-labs/harn',parent_run_id:'900',
      parent_run_attempt:2,parent_head_sha:'c'.repeat(40),source_revision:source,started_at:1000,
      dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab'};
    const parent={...run,id:900,run_attempt:2,head_sha:context.parent_head_sha,event:'workflow_dispatch',
      path:'.github/workflows/promote-release.yml'};
    const fixture=path.join(scratch,'fixture.json');
    const preload=path.join(scratch,'fetch.mjs');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const data=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
      globalThis.fetch=async(url)=>{
        const route=new URL(url).pathname;
        const record=route.endsWith('/900')?data.parent:route.endsWith('/jobs')?
          {total_count:data.jobs.length,jobs:data.jobs}:route.endsWith('/artifacts')?
          {total_count:data.artifacts.length,artifacts:data.artifacts}:data.run;
        return {ok:true,json:async()=>record};
      };`);
    const entry=fileURLToPath(new URL('../ci/consumer-observer-dispatch.mjs',import.meta.url));
    const execute=(changes={})=>{
      fs.writeFileSync(fixture,JSON.stringify({run,jobs,artifacts,parent,...changes}));
      return spawnSync(process.execPath,[entry,'--archive-producer'],{encoding:'utf8',env:{
        PATH:process.env.PATH,NODE_OPTIONS:`--import=${preload}`,GH_TOKEN:'fixture-read',PARENT_READ_TOKEN:'fixture-read',
        GITHUB_REPOSITORY:context.parent_repository,GITHUB_RUN_ID:'900',GITHUB_RUN_ATTEMPT:'2',
        GITHUB_SHA:context.parent_head_sha,OBSERVER_CONTEXT:JSON.stringify(context),SOURCE_REVISION:source,
        CONSUMER_PULL_REQUEST:'7',CANARY_RELEASE_PRODUCER_RUN_ID:'42',CANARY_RELEASE_PRODUCER_RUN_ATTEMPT:'1',
      }});
    };
    const admitted=execute();
    assert.equal(admitted.status,0,admitted.stderr);
    assert.equal(JSON.parse(admitted.stdout).producer_attempt,1);
    assert.equal(JSON.parse(admitted.stdout).producer_run,'42');
    for(const changed of [{...parent,path:'.github/workflows/ci.yml'},{...parent,run_attempt:3},
      {...parent,status:'completed',conclusion:'failure'},{...parent,head_repository:{full_name:'fork/harn'}}]) {
      assert.equal(execute({parent:changed}).status,1,'an unrelated or ended observer cannot reuse producer evidence');
    }
    assert.equal(execute({jobs:jobs.filter(job=>job.name!=='Assemble candidate manifest')}).status,1);
    for(const conclusion of ['failure','cancelled','timed_out','skipped',null]) {
      const failedAudit=jobs.map(job=>job.id===98?{...job,status:'completed',conclusion}:job);
      assert.equal(execute({jobs:failedAudit}).status,1,'recovery dispatch must reject an unsuccessful audit');
    }
    assert.equal(execute({jobs:jobs.map(job=>job.id===98?{...job,status:'in_progress',conclusion:null}:job)}).status,1);
    assert.equal(execute({artifacts:[]}).status,1);
  } finally {
    fs.rmSync(scratch,{recursive:true,force:true});
  }
});

test('the actual promotion resolver admits only an explicit repair with complete original archive proof',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-repair-resolver-'));
  try {
    const fixture=path.join(scratch,'fixture.json');
    const preload=path.join(scratch,'fetch.mjs');
    const output=path.join(scratch,'output');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const data=JSON.parse(fs.readFileSync(process.env.REPAIR_FIXTURE,'utf8'));
      globalThis.fetch=async(url)=>({ok:true,json:async()=>url.includes('/jobs')?
        {total_count:data.jobs.length,jobs:data.jobs}:url.includes('/artifacts')?
        {total_count:data.artifacts.length,artifacts:data.artifacts}:data.run});`);
    fs.writeFileSync(path.join(scratch,'gh'),`#!${process.execPath}
      const fs=require('node:fs');
      const data=JSON.parse(fs.readFileSync(process.env.REPAIR_FIXTURE,'utf8'));
      const route=process.argv[3];
      if(route.includes('/compare/')) console.log(JSON.stringify({status:'ahead',merge_base_commit:{sha:data.run.head_sha}}));
      else if(route.endsWith('/42')) console.log(JSON.stringify(data.run));
      else process.exit(2);`,{mode:0o755});
    const execute=(changes={},environment={})=>{
      fs.writeFileSync(fixture,JSON.stringify({run,jobs,artifacts,...changes}));
      fs.writeFileSync(output,'');
      const result=spawnSync('bash',[fileURLToPath(new URL('../resolve-release-promotion-source.sh',import.meta.url))],{
        encoding:'utf8',env:{PATH:`${scratch}:${process.env.PATH}`,NODE_OPTIONS:`--import=${preload}`,
          REPAIR_FIXTURE:fixture,GH_TOKEN:'fixture-read',GITHUB_REPOSITORY:'burin-labs/harn',
          CANDIDATE_RUN_ID:'42',EXPECTED_SOURCE_SHA:source,GITHUB_OUTPUT:output,CONSUMER_PULL_REQUEST:'7',...environment},
      });
      return {...result,outputs:fs.readFileSync(output,'utf8')};
    };
    const positive=execute();
    assert.equal(positive.status,0,positive.stderr);
    assert.match(positive.outputs,/requires_consumer_repair=true\n/);
    assert.match(positive.outputs,/producer_attempt=1\n/);
    const failed=execute({run:{...run,status:'completed',conclusion:'failure'}});
    assert.equal(failed.status,0,failed.stderr);
    for(const [changes,environment] of [
      [{},{CONSUMER_PULL_REQUEST:''}], [{},{CONSUMER_PULL_REQUEST:'other'}],
      [{},{COMPLETED_CONSUMER_RUN_ID:'123'}],
      [{run:{...run,status:'completed',conclusion:'cancelled'}},{}],
      [{jobs:jobs.filter(job=>job.name!=='Assemble candidate manifest')},{}],
      [{jobs:jobs.filter(job=>job.id!==98)},{}],
      ...['failure','cancelled','timed_out','skipped',null].map(conclusion=>
        [{jobs:jobs.map(job=>job.id===98?{...job,status:'completed',conclusion}:job)},{}]),
      [{jobs:jobs.map(job=>job.id===98?{...job,status:'in_progress',conclusion:null}:job)},{}],
      [{artifacts:[]},{}], [{run:{...run,head_repository:{full_name:'fork/harn'}}},{}],
    ]) {
      const refused=execute(changes,environment);
      assert.notEqual(refused.status,0);
      assert.equal(refused.outputs,'','unmeasured source must emit no promotion outputs');
    }
  } finally {
    fs.rmSync(scratch,{recursive:true,force:true});
  }
});
