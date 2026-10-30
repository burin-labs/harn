import assert from 'node:assert/strict';
import {test} from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {consumerDeadlineReceipt,legacyConsumerObservationPolicy as consumerObservationPolicy,qualifyConsumerDeadline,observerDeadlineStep,
  observerDeadlineFilename,observerDeadlineName,observerWindowSteps,waitLegacyConsumerWindow as waitConsumerWindow} from '../lib/consumer-observer-window.mjs';
import {observerDeclarationName,observerDeclarationStep,observerPublicationWitness} from '../lib/consumer-observer.mjs';

test('the actual deadline reader authenticates real archives and original producer steps before recovery evidence',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-observer-deadline-reader-'));
  try{
    const context={schema:'harn.consumer_observer.v2',parent_repository:'burin-labs/harn',parent_run_id:'42',
      parent_run_attempt:2,parent_head_sha:'a'.repeat(40),source_revision:'b'.repeat(40),
      dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab',started_at:1000};
    const parent={id:42,run_attempt:2,head_sha:context.parent_head_sha,head_repository:{full_name:context.parent_repository},
      path:'.github/workflows/build-release-binaries.yml',status:'completed',conclusion:'failure'};
    const reading={discovered:1,owned:1,pending:1,refused:0,custody_commitment:'c'.repeat(64),execution:null,product:null};
    const timestamp=seconds=>new Date(seconds*1000).toISOString();
    const step=(name,number,start,end,conclusion)=>({name,number,status:'completed',conclusion,
      started_at:timestamp(start),completed_at:timestamp(end)});
    const declared=step(observerDeclarationStep,2,999,1001,'success');
    const observed=step(observerWindowSteps[2],8,8100,8201,'failure');
    const published=step(observerDeadlineStep,9,8201,8202,'success');
    const job={id:701,run_id:42,run_attempt:2,head_sha:context.parent_head_sha,status:'completed',conclusion:'failure',
      steps:[declared,step(observerPublicationWitness(context),3,1001,1002,'success'),observed,published]};
    const fixture=path.join(scratch,'fixture.json'),preload=path.join(scratch,'fetch.mjs');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const fixture=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
      let parentReads=0;
      globalThis.fetch=async(url,options={})=>{
        if(options.method==='POST')throw new Error('unexpected-write');
        const endpoint=new URL(url),p=endpoint.pathname;
        if(!p.startsWith('/repos/burin-labs/harn/actions/'))throw new Error('foreign-read');
        if(p.endsWith('/42'))return {ok:true,json:async()=>++parentReads>1&&fixture.finalParent?fixture.finalParent:fixture.parent};
        if(p.endsWith('/attempts/2/jobs'))return {ok:true,json:async()=>({total_count:fixture.partial?fixture.jobs.length+1:fixture.jobs.length,jobs:fixture.jobs})};
        if(p.endsWith('/artifacts')){
          const wanted=endpoint.searchParams.get('name');
          if(!wanted)throw new Error('unfiltered-read');
          const selected=fixture.artifacts.filter(a=>a.metadata.name===wanted);
          return {ok:true,json:async()=>({total_count:selected.length,artifacts:selected.map(a=>a.metadata)})};
        }
        if(p.endsWith('/zip')){
          const item=fixture.artifacts.find(a=>p.endsWith('/'+a.metadata.id+'/zip'));
          if(!item)throw new Error('unknown-artifact');
          return {ok:true,arrayBuffer:async()=>fs.readFileSync(item.archive)};
        }
        throw new Error('unknown-url');
      };`);
    let sequence=0;
    const execute=(change={})=>{
      const issued=change.context??context;
      const deadline=change.receipt??consumerDeadlineReceipt(issued,8200,reading);
      const directory=path.join(scratch,String(sequence++));fs.mkdirSync(directory);
      const artifacts=[[observerDeclarationName(issued),'consumer-observer.json',issued],
        [observerDeadlineName(issued),observerDeadlineFilename,deadline]].map(([name,filename,value],index)=>{
        const file=path.join(directory,filename),archive=path.join(directory,`${index}.zip`);
        fs.writeFileSync(file,JSON.stringify(value));
        const zip=spawnSync('zip',['-j',archive,file],{env:{PATH:process.env.PATH,LC_ALL:'C'},encoding:'utf8'});
        assert.equal(zip.status,0,zip.stderr);
        const bytes=fs.readFileSync(archive);
        return {archive,metadata:{id:501+index,name,expired:false,size_in_bytes:bytes.length,
          digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,
          workflow_run:{id:42,head_sha:context.parent_head_sha}}};
      });
      if(change.missing)artifacts.pop();
      if(change.duplicate)artifacts.push(artifacts[1]);
      if(change.foreign)artifacts[1].metadata.workflow_run.id=43;
      fs.writeFileSync(fixture,JSON.stringify({parent,jobs:[job],
        ...change,artifacts}));
      const entry=fileURLToPath(new URL('../ci/consumer-observer-read-decision.mjs',import.meta.url));
      return spawnSync(process.execPath,[entry,'--deadline'],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${preload}`,GH_TOKEN:'fixture-public',OBSERVER_CONTEXT:JSON.stringify(issued),
        SOURCE_REVISION:context.source_revision,REQUIRE_CURRENT_DEFAULT:'false',...change.env}});
    };
    const good=execute();assert.equal(good.status,0,good.stderr);
    assert.deepEqual(JSON.parse(good.stdout),{context,deadline:consumerDeadlineReceipt(context,8200,reading),owned:false});
    for(const change of [{missing:true},{duplicate:true},{foreign:true},{partial:true},
      {env:{REQUIRE_CURRENT_DEFAULT:'true'}},{env:{SOURCE_REVISION:'c'.repeat(40)}},
      {finalParent:{...parent,run_attempt:3}},{finalParent:{...parent,head_sha:'c'.repeat(40)}},
      {context:{...context,started_at:900}},
      {receipt:{...consumerDeadlineReceipt(context,8200,reading),reading:{...reading,product:{verdict:'pass'}}}},
      {jobs:[{...job,run_id:43}]},{jobs:[{...job,run_attempt:3}]},
      {jobs:[{...job,head_sha:'c'.repeat(40)}]},{jobs:[{...job,run_id:null}]},
      {jobs:[{...job,steps:[declared,observed,{...published,conclusion:'skipped'}]}]},
      {jobs:[{...job,steps:[observed,published]},
        {id:702,conclusion:'success',steps:[declared]}]}]){
      const bad=execute(change);assert.equal(bad.status,1,bad.stdout);assert.equal(bad.stdout,'');
    }
  }finally{fs.rmSync(scratch,{recursive:true});}
});

test('deadline custody refuses invented starts, unrelated producers and absent or late product evidence',()=>{
  const context={schema:'harn.consumer_observer.v2',parent_repository:'burin-labs/harn',parent_run_id:'42',
    parent_run_attempt:2,parent_head_sha:'a'.repeat(40),source_revision:'b'.repeat(40),
    dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab',started_at:1000};
  const pending={discovered:1,owned:1,pending:1,refused:0,custody_commitment:'c'.repeat(64),execution:null,product:null};
  const receipt=consumerDeadlineReceipt(context,8200,pending);
  const timestamp=seconds=>new Date(seconds*1000).toISOString();
  const step=(name,number,start,end,conclusion)=>({name,number,status:'completed',conclusion,
    started_at:timestamp(start),completed_at:timestamp(end)});
  const declared=step(observerDeclarationStep,2,999,1001,'success');
  const observed=step(observerWindowSteps[2],8,8100,8201,'failure');
  const published=step(observerDeadlineStep,9,8201,8202,'success');
  assert.deepEqual(qualifyConsumerDeadline(receipt,context,declared,observed,published),receipt);
  for(const reading of [null,{...pending,discovered:0,owned:0},{...pending,refused:1},
    {...pending,execution:'failure'},{...pending,product:{verdict:'pass'}},
    {...pending,product:{verdict:'fail'}},{...pending,pending:0}]){
    assert.throws(()=>consumerDeadlineReceipt(context,8200,reading),/deadline_unmeasured/);
  }
  assert.throws(()=>consumerDeadlineReceipt(context,8199,pending),/deadline_unmeasured/);
  for(const change of [{parent_run_attempt:3},{source_revision:'c'.repeat(40)}]){
    assert.throws(()=>qualifyConsumerDeadline(receipt,{...context,...change},declared,observed,published),/deadline_identity_unproven/);
  }
  const backdated={...context,started_at:900};
  assert.throws(()=>qualifyConsumerDeadline(consumerDeadlineReceipt(backdated,8200,pending),backdated,
    declared,observed,published),/deadline_producer_clock_unproven/);
  assert.throws(()=>qualifyConsumerDeadline(receipt,context,declared,
    {...observed,completed_at:timestamp(8199)},published),/deadline_producer_clock_unproven/);
  for(const publication of [{...published,started_at:timestamp(8199)},
    {...published,completed_at:null},{...published,completed_at:timestamp(8200)}]){
    assert.throws(()=>qualifyConsumerDeadline(receipt,context,declared,observed,publication),/deadline_producer_clock_unproven/);
  }
  for(const publication of [{...published,name:'unrelated artifact'}, {...published,conclusion:'skipped'},
    {...published,number:1}]){
    assert.throws(()=>qualifyConsumerDeadline(receipt,context,declared,observed,publication),/deadline_identity_unproven/);
  }
});

test('legacy credential renewal retains its original budget and cannot manufacture a product verdict',async()=>{
  const policy=consumerObservationPolicy();
  assert.equal(policy.max_windows,3);
  let clock=1000,calls=0;
  const pending={discovered:0,owned:0,pending:1,refused:0,execution:null,product:null};
  const instrument={now:()=>clock,pause:async seconds=>{clock+=seconds;},policy};
  const observe=async()=>{calls++;return pending;};
  const first=await waitConsumerWindow(1000,observe,instrument);
  assert.equal(first.verdict,'pending');assert.equal(first.wallSeconds,2700);
  const second=await waitConsumerWindow(1000,observe,instrument);
  assert.equal(second.verdict,'pending');assert.equal(second.wallSeconds,5400);
  const last=await waitConsumerWindow(1000,observe,instrument);
  assert.equal(last.verdict,'deadline');assert.equal(last.wallSeconds,7200);
  assert(calls>0,'a measured pending window must reach the observer');
  const expiredCalls=calls;
  assert.equal((await waitConsumerWindow(1000,observe,instrument)).verdict,'deadline');
  assert.equal(calls,expiredCalls+1,'deadline evidence requires a fresh authenticated observation');

  clock=1000;
  const missing={...pending,discovered:1,owned:1,execution:'success'};
  const successfulExecution=await waitConsumerWindow(1000,async()=>missing,instrument);
  assert.equal(successfulExecution.verdict,'pending');assert.equal(successfulExecution.reading.product,null);
  clock=1000;
  const failedExecution=await waitConsumerWindow(1000,async()=>({...missing,pending:0,execution:'failure'}),instrument);
  assert.equal(failedExecution.verdict,'execution_failed');assert.equal(failedExecution.reading.product,null);
  for(const verdict of ['pass','fail']){
    const measured=await waitConsumerWindow(1000,async()=>({...missing,pending:0,product:{verdict}}),instrument);
    assert.equal(measured.verdict,verdict);
  }
  const refused=await waitConsumerWindow(1000,async()=>({...missing,pending:0,refused:1}),instrument);
  assert.equal(refused.verdict,'refused');
  await assert.rejects(waitConsumerWindow(1001,observe,instrument),/observation_start_invalid/);
  const late=await waitConsumerWindow(1000,async()=>{clock=8200;return {...missing,pending:0,product:{verdict:'pass'}};},instrument);
  assert.equal(late.verdict,'deadline','late positive evidence cannot reset the deadline');
});
