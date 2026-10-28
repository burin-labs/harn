import assert from 'node:assert/strict';
import test from 'node:test';
import {execFileSync,spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {githubArchiveProducer,qualifyArchiveProducer} from '../lib/consumer-observer.mjs';

const targets=JSON.parse(execFileSync('bash',[fileURLToPath(new URL('../lib/candidate_archive_contract.sh',import.meta.url)),'--targets-json'],{encoding:'utf8'}));
const source='a'.repeat(40);
const request={sourceRevision:source,runId:'42',attempt:1};
const run={id:42,run_attempt:1,head_sha:source,head_branch:'main',event:'push',status:'in_progress',conclusion:null,
  repository:{full_name:'burin-labs/harn'},head_repository:{full_name:'burin-labs/harn'},path:'.github/workflows/build-release-binaries.yml'};
const names=['Resolve release context','Prepare CLI AOT payload','Assemble candidate manifest','Release residual audit',
  'Consumer release rehearsal / Decide whether main moved',...targets.map(target=>`Build ${target}`),
  ...['linux','linux-arm64','windows','macos'].map(target=>`Release smoke / Release smoke (${target})`)];
const jobs=names.map((name,index)=>({id:index+1,run_id:42,run_attempt:1,head_sha:source,name,status:'completed',conclusion:'success'}));
jobs.push({id:99,run_id:42,run_attempt:1,head_sha:source,name:'Consumer release rehearsal / Consumer canary',status:'in_progress',conclusion:null});
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

test('a historical source is bound to its main policy run by the producer manifest',()=>{
  const policy='c'.repeat(40);
  const historicalRun={...run,head_sha:policy,event:'workflow_dispatch',status:'completed',conclusion:'success'};
  const historicalJobs=jobs.filter(job=>!job.name.startsWith('Consumer release rehearsal /'))
    .map(job=>({...job,head_sha:policy,status:'completed',conclusion:'success'}));
  historicalJobs.push(...['Qualify candidate source','Qualify native release source','Release candidate verdict']
    .map((name,index)=>({id:200+index,run_id:42,run_attempt:1,head_sha:policy,name,status:'completed',conclusion:'success'})));
  historicalJobs.push({id:203,run_id:42,run_attempt:1,head_sha:policy,name:'Consumer release rehearsal',status:'completed',conclusion:'skipped'});
  const historicalArtifacts=artifacts.map(artifact=>({...artifact,workflow_run:{id:42,head_sha:policy}}));
  const manifest={schemaVersion:'burin-labs.candidate_manifest.v1',repository:'burin-labs/harn',
    sourceCommit:source,runId:'42',runAttempt:'1'};
  assert.equal(qualifyArchiveProducer(historicalRun,historicalJobs,historicalArtifacts,request,targets,manifest).verdict,'pass');
  for(const changed of [null,{...manifest,sourceCommit:policy},{...manifest,runId:'43'},
    {...manifest,runAttempt:'2'},{...manifest,repository:'fork/harn'},{...manifest,schemaVersion:'unknown'}]) {
    assert.throws(()=>qualifyArchiveProducer(historicalRun,historicalJobs,historicalArtifacts,request,targets,changed),/identity_unproven/);
  }
  for(const changed of [{...historicalRun,event:'schedule'},{...historicalRun,head_branch:'topic'},
    {...historicalRun,status:'in_progress',conclusion:null}]) {
    assert.throws(()=>qualifyArchiveProducer(changed,historicalJobs,historicalArtifacts,request,targets,manifest),/identity_unproven/);
  }
  for(const required of ['Qualify candidate source','Qualify native release source','Release candidate verdict','Consumer release rehearsal']) {
    assert.throws(()=>qualifyArchiveProducer(historicalRun,historicalJobs.filter(job=>job.name!==required),historicalArtifacts,request,targets,manifest),/builds_unproven/);
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

test('the historical producer reader authenticates the manifest bytes and main ancestry',async()=>{
  const policy='c'.repeat(40);
  const producer={...run,head_sha:policy,event:'workflow_dispatch',status:'completed',conclusion:'success',
    repository:{full_name:'burin-labs/harn',id:9},head_repository:{full_name:'burin-labs/harn',id:9}};
  const producerJobs=jobs.filter(job=>!job.name.startsWith('Consumer release rehearsal /'))
    .map(job=>({...job,head_sha:policy,status:'completed',conclusion:'success'}));
  producerJobs.push(...['Qualify candidate source','Qualify native release source','Release candidate verdict']
    .map((name,index)=>({id:200+index,run_id:42,run_attempt:1,head_sha:policy,name,status:'completed',conclusion:'success'})));
  producerJobs.push({id:203,run_id:42,run_attempt:1,head_sha:policy,name:'Consumer release rehearsal',status:'completed',conclusion:'skipped'});
  const manifest={schemaVersion:'burin-labs.candidate_manifest.v1',repository:'burin-labs/harn',
    sourceCommit:source,runId:'42',runAttempt:'1'};
  const bytes=Buffer.from(JSON.stringify(manifest));
  const digest=`sha256:${createHash('sha256').update(bytes).digest('hex')}`;
  const producerArtifacts=artifacts.map(artifact=>({...artifact,digest,workflow_run:{id:42,head_sha:policy,
    head_branch:'main',repository_id:9,head_repository_id:9}}));
  const read=({ancestor=source,archive=bytes,listed=producerArtifacts,observedJobs=producerJobs}={})=>async(url)=>{
    if(url.includes('/compare/')) return {ok:true,json:async()=>({status:'ahead',merge_base_commit:{sha:ancestor}})};
    if(url.endsWith('/zip')) return {ok:true,arrayBuffer:async()=>archive};
    const record=url.includes('/jobs')?{total_count:observedJobs.length,jobs:observedJobs}:
      url.includes('/artifacts')?{total_count:listed.length,artifacts:listed}:producer;
    return {ok:true,json:async()=>record};
  };
  const extract=data=>JSON.parse(data.toString());
  assert.equal((await githubArchiveProducer('fixture-read',request,read(),extract)).verdict,'pass');
  await assert.rejects(githubArchiveProducer('fixture-read',request,read({ancestor:policy}),extract),/identity_unproven/);
  await assert.rejects(githubArchiveProducer('fixture-read',request,read({archive:Buffer.from('{}')}),extract),/artifacts_unproven/);
  await assert.rejects(githubArchiveProducer('fixture-read',request,read({observedJobs:producerJobs.filter(job=>job.name!=='Qualify native release source')}),extract),/builds_unproven/);
  const changed=producerArtifacts.map(artifact=>artifact.name.startsWith('candidate-manifest-')?
    {...artifact,workflow_run:{...artifact.workflow_run,repository_id:10}}:artifact);
  await assert.rejects(githubArchiveProducer('fixture-read',request,read({listed:changed}),extract),/artifacts_unproven/);
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

test('the actual promotion resolver binds historical archives and still requires a fresh consumer repair',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-historical-resolver-'));
  try {
    const policy='c'.repeat(40);
    const historical={...run,head_sha:policy,event:'workflow_dispatch',status:'completed',conclusion:'success',
      repository:{full_name:'burin-labs/harn',id:9},head_repository:{full_name:'burin-labs/harn',id:9}};
    const historicalJobs=jobs.filter(job=>!job.name.startsWith('Consumer release rehearsal /'))
      .map(job=>({...job,head_sha:policy,status:'completed',conclusion:'success'}));
    historicalJobs.push(...['Qualify candidate source','Qualify native release source','Release candidate verdict']
      .map((name,index)=>({id:200+index,run_id:42,run_attempt:1,head_sha:policy,name,status:'completed',conclusion:'success'})));
    historicalJobs.push({id:203,run_id:42,run_attempt:1,head_sha:policy,name:'Consumer release rehearsal',status:'completed',conclusion:'skipped'});
    fs.writeFileSync(path.join(scratch,'candidate-manifest.json'),JSON.stringify({
      schemaVersion:'burin-labs.candidate_manifest.v1',repository:'burin-labs/harn',sourceCommit:source,runId:'42',runAttempt:'1'}));
    execFileSync('zip',['-q','manifest.zip','candidate-manifest.json'],{cwd:scratch});
    const archive=fs.readFileSync(path.join(scratch,'manifest.zip'));
    const digest=`sha256:${createHash('sha256').update(archive).digest('hex')}`;
    const historicalArtifacts=artifacts.map(artifact=>({...artifact,digest,workflow_run:{id:42,head_sha:policy,
      head_branch:'main',repository_id:9,head_repository_id:9}}));
    const fixture=path.join(scratch,'fixture.json');
    const preload=path.join(scratch,'fetch.mjs');
    const output=path.join(scratch,'output');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const d=JSON.parse(fs.readFileSync(process.env.REPAIR_FIXTURE,'utf8'));
      globalThis.fetch=async(url)=>{
        if(url.includes('/compare/')) return {ok:true,json:async()=>({status:'ahead',merge_base_commit:{sha:d.ancestor}})};
        if(url.endsWith('/zip')) return {ok:true,arrayBuffer:async()=>Buffer.from(d.archive,'base64')};
        return {ok:true,json:async()=>url.includes('/jobs')?{total_count:d.jobs.length,jobs:d.jobs}:
          url.includes('/artifacts')?{total_count:d.artifacts.length,artifacts:d.artifacts}:d.run};
      };`);
    fs.writeFileSync(path.join(scratch,'gh'),`#!${process.execPath}
      const fs=require('node:fs');
      const d=JSON.parse(fs.readFileSync(process.env.REPAIR_FIXTURE,'utf8'));
      const route=process.argv[3];
      if(route.includes('/compare/')) console.log(JSON.stringify({status:'ahead',merge_base_commit:{sha:d.ancestor}}));
      else if(route.includes('/contents/Cargo.toml')) console.log('version = "0.10.181"');
      else if(route.includes('/artifacts')) console.log(JSON.stringify({total_count:d.artifacts.length,artifacts:d.artifacts}));
      else if(route.endsWith('/42')) console.log(JSON.stringify(d.run));
      else process.exit(2);`,{mode:0o755});
    const execute=(changes={},repair='7')=>{
      fs.writeFileSync(fixture,JSON.stringify({run:historical,jobs:historicalJobs,artifacts:historicalArtifacts,
        ancestor:source,archive:archive.toString('base64'),...changes}));
      fs.writeFileSync(output,'');
      const result=spawnSync('bash',[fileURLToPath(new URL('../resolve-release-promotion-source.sh',import.meta.url))],{
        encoding:'utf8',env:{PATH:`${scratch}:${process.env.PATH}`,NODE_OPTIONS:`--import=${preload}`,
          REPAIR_FIXTURE:fixture,GH_TOKEN:'fixture-read',GITHUB_REPOSITORY:'burin-labs/harn',
          CANDIDATE_RUN_ID:'42',EXPECTED_SOURCE_SHA:source,GITHUB_OUTPUT:output,CONSUMER_PULL_REQUEST:repair},
      });
      return {...result,outputs:fs.readFileSync(output,'utf8')};
    };
    const admitted=execute();
    assert.equal(admitted.status,0,admitted.stderr);
    assert.match(admitted.outputs,new RegExp(`source_sha=${source}\\n`));
    assert.match(admitted.outputs,/requires_consumer_repair=true\n/);
    assert.notEqual(execute({},'').status,0);
    assert.notEqual(execute({ancestor:policy}).status,0);
    assert.notEqual(execute({archive:Buffer.from('{}').toString('base64')}).status,0);
    assert.notEqual(execute({jobs:historicalJobs.filter(job=>job.name!=='Qualify native release source')}).status,0);
  } finally {
    fs.rmSync(scratch,{recursive:true,force:true});
  }
});
