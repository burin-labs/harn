import assert from 'node:assert/strict';
import test from 'node:test';
import {execFileSync,spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
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
