import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync,spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {observerDeadlineName,observerDeadlineStep,observerWindowSteps} from '../lib/consumer-observer-window.mjs';
import {admitChild, createTerminalReceipt, qualifyOwnedChild, observerDeclarationName, observerDeclarationStep, observerDispatchActor, observerRunName, observerTerminalName} from '../lib/consumer-observer.mjs';

test('the terminal entrypoint records only an authenticated reached product decision', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-terminal-'));
  try {
    const context = {schema:'harn.consumer_observer.v2',parent_repository:'burin-labs/harn',
      parent_run_id:'42',parent_run_attempt:2,parent_head_sha:'a'.repeat(40),
      source_revision:'b'.repeat(40),dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab',started_at:1000};
    const parent = {id:42,run_attempt:2,head_sha:context.parent_head_sha,
      head_repository:{full_name:context.parent_repository},path:'.github/workflows/consumer-canary.yml',
      status:'in_progress'};
    const child = {id:73,run_attempt:1,head_sha:'c'.repeat(40),head_repository:{full_name:'acme/widget-host'},
      path:'.github/workflows/rehearsal.yml',head_branch:'main',event:'workflow_dispatch',status:'in_progress',
      actor:{login:observerDispatchActor,type:'Bot'},display_title:observerRunName(context)};
    const expected={repository:'acme/widget-host',workflow:child.path,runId:'73',attempt:1,
      headSha:child.head_sha,sourceRevision:context.source_revision};
    const ownership=admitChild(context,parent,expected,child,{inputs:{source_revision:context.source_revision,
      observer:JSON.stringify(context)}},context);
    const commitment=createHash('sha256').update(JSON.stringify(qualifyOwnedChild(ownership,context,parent,child,
      expected,context))).digest('hex');
    ownership.child=Object.fromEntries(Object.entries(ownership.child).reverse());
    const name = `consumer-observer-${context.dispatch_nonce}`;
    const archives = [];
    for (const [index,filename,value] of [[0,'consumer-observer.json',context],[1,`${name}.json`,ownership]]) {
      const file = path.join(scratch,filename);
      const archive = path.join(scratch,`archive-${index}.zip`);
      fs.writeFileSync(file,JSON.stringify(value));
      const zipped = spawnSync('zip',['-j',archive,file],{env:{PATH:process.env.PATH,LC_ALL:'C'},encoding:'utf8'});
      assert.equal(zipped.status,0,zipped.stderr);
      const bytes = fs.readFileSync(archive);
      archives.push({archive,metadata:{id:501+index,name:index===0?observerDeclarationName(context):name,
        expired:false,size_in_bytes:bytes.length,digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,
        workflow_run:{id:index===0?42:73,head_sha:index===0?parent.head_sha:child.head_sha}}});
    }
    const fixture = path.join(scratch,'fixture.json');
    const preload = path.join(scratch,'fetch.mjs');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const fixture=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
      globalThis.fetch=async(url,options={})=>{
        if(options.method==='POST')throw new Error('unexpected-private-post');
        const endpoint=new URL(url),p=endpoint.pathname;
        if(p.endsWith('/zip'))return {ok:true,arrayBuffer:async()=>fs.readFileSync(fixture.archives[p.includes('/501/')?0:1].archive)};
        let record;
        if(p.endsWith('/artifacts')){
          const index=p.includes('/42/')?0:1;
          if(!endpoint.searchParams.has('name'))throw new Error('unfiltered-private-read');
          record={total_count:fixture.missing===index?0:1,artifacts:fixture.missing===index?[]:[fixture.archives[index].metadata]};
        }else if(p.endsWith('/42/attempts/2'))record=fixture.parent;
        else if(p.endsWith('/73'))record=fixture.child;
        else throw new Error('unknown-private-url');
        return {ok:true,json:async()=>record};
      };`);
    const product = {source_revision:context.source_revision,verdict:'pass',owed:2,proven:2,pending:0,failed:0,failed_suites:[]};
    const entry = fileURLToPath(new URL('../ci/consumer-observer-terminal.mjs',import.meta.url));
    let sequence=0;
    const execute = (changes={},decision=product,extraEnv={}) => {
      const directory=path.join(scratch,`case-${sequence++}`);
      fs.mkdirSync(directory);
      const output=path.join(directory,'output'),decisionFile=path.join(directory,'decision.json');
      fs.writeFileSync(fixture,JSON.stringify({parent,child,archives,...changes}));
      if(decision!==null)fs.writeFileSync(decisionFile,JSON.stringify(decision));
      const run=spawnSync(process.execPath,[entry],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${preload}`,GH_TOKEN:'fixture-read',OBSERVER_CONTEXT:JSON.stringify(context),
        GITHUB_REPOSITORY:'acme/widget-host',GITHUB_RUN_ID:'73',GITHUB_RUN_ATTEMPT:'1',GITHUB_SHA:child.head_sha,
        GITHUB_WORKFLOW_REF:`acme/widget-host/${child.path}@refs/heads/main`,RUNNER_TEMP:directory,
        GITHUB_OUTPUT:output,PRODUCT_DECISION_FILE:decisionFile,...extraEnv}});
      assert.doesNotMatch(run.stdout+run.stderr,/widget-host|\/73|run_id|private/);
      return {run,output:fs.existsSync(output)?fs.readFileSync(output,'utf8'):''};
    };
    const positive=execute();
    assert.equal(positive.run.status,0,positive.run.stderr);
    assert.match(positive.run.stdout,/product=recorded owed=2 proven=2 pending=0 failed=0/);
    const recorded=Object.fromEntries(positive.output.trim().split('\n').map(line=>{
      const split=line.indexOf('=');return [line.slice(0,split),line.slice(split+1)];
    }));
    assert.equal(recorded.recorded,'true');
    assert.equal(recorded.artifact,observerTerminalName(context));
    const receipt=JSON.parse(fs.readFileSync(recorded.receipt,'utf8'));
    assert.equal(receipt.schema,'harn.consumer_observer_terminal.v2');
    assert.deepEqual(receipt.product,product);
    assert.deepEqual(receipt.child,ownership.child);
    assert.notDeepEqual(Object.keys(receipt.child),Object.keys(ownership.child),
      'the actual producer normalizes equivalent reordered custody once');
    assert.equal(fs.statSync(recorded.receipt).mode&0o777,0o600);
    const failedProduct={...product,verdict:'fail',proven:1,failed:1,failed_suites:['harn-linked']};
    const failedProducer=execute({},failedProduct);
    assert.equal(failedProducer.run.status,0,failedProducer.run.stderr);
    assert.match(failedProducer.run.stdout,/owed=2 proven=1 pending=0 failed=1/);
    for(const decision of [null,{}, {...product,owed:0,proven:0}, {...product,proven:1},
      {...product,pending:1}, {...product,failed:1}, {...product,source_revision:'d'.repeat(40)},
      {...product,extra:'unmeasured'}, {...failedProduct,failed_suites:[]},
      {...failedProduct,failed_suites:['harn-linked','harn-linked']},
      {...failedProduct,verdict:'pass'}, {...failedProduct,proven:2},
      {...failedProduct,failed_suites:['']}, {...failedProduct,pending:1}]){
      const negative=execute({},decision);
      assert.equal(negative.run.status,1);
      assert.equal(negative.output,'');
    }
    for(const changes of [{missing:0},{missing:1},{child:{...child,run_attempt:2}},
      {child:{...child,head_sha:'d'.repeat(40)}},{parent:{...parent,run_attempt:3}}]){
      const negative=execute(changes);
      assert.equal(negative.run.status,1);
      assert.equal(negative.output,'');
    }
    const wrongAttempt=execute({},product,{GITHUB_RUN_ATTEMPT:'2'});
    assert.equal(wrongAttempt.run.status,1);
    assert.match(wrongAttempt.run.stderr,/producer_attempt_unproven/);
    assert.equal(wrongAttempt.output,'');
    const ordinary=execute({},null,{OBSERVER_CONTEXT:''});
    assert.equal(ordinary.run.status,0,ordinary.run.stderr);
    assert.equal(ordinary.output,'recorded=false\n');

    // Observe the receipt actually produced above, rather than a fabricated
    // green counter. The consumer must subsequently complete successfully.
    const archive=path.join(scratch,'archive-2.zip');
    const zipped=spawnSync('zip',['-j',archive,recorded.receipt],{env:{PATH:process.env.PATH,LC_ALL:'C'},encoding:'utf8'});
    assert.equal(zipped.status,0,zipped.stderr);
    const bytes=fs.readFileSync(archive);
    archives.push({archive,metadata:{id:503,name:observerTerminalName(context),expired:false,
      size_in_bytes:bytes.length,digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,
      workflow_run:{id:73,head_sha:child.head_sha}}});
    const observerPreload=path.join(scratch,'observe-fetch.mjs');
    const lastRead=path.join(scratch,'last-read.json');
    fs.writeFileSync(observerPreload,`import fs from 'node:fs';
      const fixture=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
      let censusReads=0,childReads=0,parentReads=0,sourceReads=0,pullReads=0;
      if(fixture.recordedClock){
        let seconds=fixture.clockStart??1000;
        Date.now=()=>seconds*1000;
        globalThis.setTimeout=(callback,milliseconds)=>{seconds+=milliseconds/1000;queueMicrotask(callback);};
      }
      globalThis.fetch=async(url,options={})=>{
        if(options.method==='POST')throw new Error('unexpected-private-post');
        const endpoint=new URL(url),p=endpoint.pathname;
        fs.writeFileSync(${JSON.stringify(lastRead)},JSON.stringify(p));
        if(fixture.producerData && p.includes('/runs/900')){
          const data=fixture.producerData;
          const record=p.endsWith('/jobs')?{total_count:data.jobs.length,jobs:data.jobs}:
            p.endsWith('/artifacts')?{total_count:data.artifacts.length,artifacts:data.artifacts}:data.run;
          return {ok:true,json:async()=>record};
        }
        if(p.endsWith('/zip')){
          const index=Number(/artifacts\\/(50[1234])\\//.exec(p)?.[1])-501;
          return {ok:true,arrayBuffer:async()=>fs.readFileSync(fixture.archives[index].archive)};
        }
        let record;
        if(p.endsWith('/artifacts')){
          const wanted=endpoint.searchParams.get('name');
          if(!wanted&&!p.includes('/42/'))throw new Error('unfiltered-private-read');
          const selected=fixture.archives.filter((a,index)=>(wanted?a.metadata.name===wanted:[0,3].includes(index))&&fixture.missing!==index);
          record={total_count:selected.length,artifacts:selected.map(a=>a.metadata)};
        }else if(p.endsWith('/42/attempts/2'))record=fixture.parent;
        else if(p.endsWith('/42/attempts/2/jobs'))record={total_count:1,jobs:fixture.parentJobs??[{id:701,conclusion:'success',steps:[{
          name:${JSON.stringify(observerDeclarationStep)},status:'completed',conclusion:fixture.publication??'success'}]}]};
        else if(p.endsWith('/'+fixture.child.id+'/attempts/'+fixture.child.run_attempt+'/jobs'))record={total_count:fixture.chronologyPartial?fixture.childJobs.length+1:fixture.childJobs.length,
          jobs:fixture.childJobs};
        else if(p.endsWith('/42'))record=++parentReads>(fixture.ordinaryObservation?0:1)&&fixture.finalParent?fixture.finalParent:fixture.parent;
        else if(p==='/repos/acme/widget-host')record={full_name:'acme/widget-host',default_branch:'main'};
        else if(p.endsWith('/commits/main'))record={sha:++sourceReads>1&&fixture.finalSource?fixture.finalSource:fixture.source??'c'.repeat(40)};
        else if(p.endsWith('/pulls/7'))record=++pullReads>1&&fixture.finalPull?fixture.finalPull:fixture.pull;
        else if(p.endsWith('/'+fixture.child.id))record=++childReads>1&&fixture.finalChild?fixture.finalChild:fixture.child;
        else if(p.endsWith('/rehearsal.yml/runs')){
          const rows=++censusReads>1&&fixture.finalRuns?fixture.finalRuns:fixture.runs;
          record={total_count:fixture.partial||(censusReads>1&&fixture.finalPartial)?rows.length+1:rows.length,workflow_runs:rows};
        }else throw new Error('unknown-private-url');
        return {ok:true,json:async()=>record,headers:new Headers()};
      };`);
    const completed={...child,status:'completed',conclusion:'success'};
    const observeEntry=fileURLToPath(new URL('../ci/consumer-observer-observe.mjs',import.meta.url));
    const waitEntry=fileURLToPath(new URL('../ci/consumer-observer-wait.mjs',import.meta.url));
    const observe=(changes={},wait=false,extraEnv={})=>{
      const output=path.join(scratch,`observation-${sequence++}`);
      fs.writeFileSync(fixture,JSON.stringify({parent:{...parent,created_at:'2026-10-11T20:00:00Z'},
        child:completed,runs:[completed],archives,ordinaryObservation:true,...changes}));
      const run=spawnSync(process.execPath,[wait?waitEntry:observeEntry],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${observerPreload}`,GH_TOKEN:'fixture-read',OBSERVER_CONTEXT:JSON.stringify(context),
        CANARY_OWNER:'acme',CANARY_NAME:'widget-host',CANARY_WORKFLOW:'rehearsal.yml',GITHUB_OUTPUT:output,
        CANARY_REPOSITORY:'acme/widget-host',RUNNER_TEMP:scratch,...extraEnv}});
      const text=fs.existsSync(output)?fs.readFileSync(output,'utf8'):'';
      assert.doesNotMatch(run.stdout+run.stderr+text,/widget-host|run_id|head_sha|dispatch_actor|\/73/);
      assert(!(run.stdout+run.stderr+text).includes(ownership.child.receipt_nonce));
      const fields=Object.fromEntries(text.trim().split('\n').filter(Boolean).map(line=>{
        const split=line.indexOf('=');return [line.slice(0,split),line.slice(split+1)];
      }));
      return {run,fields,reading:fields.reading?JSON.parse(fields.reading):null};
    };
    const measured=observe();
    assert.equal(measured.run.status,0,measured.run.stderr);
    assert.equal(measured.fields.verdict,'pass');
    assert.deepEqual(measured.reading,{discovered:1,owned:1,pending:0,refused:0,custody_commitment:commitment,execution:'success',product});
    for(const changes of [{runs:[]},{runs:[completed,{...completed,id:74}]},{missing:1},{missing:2},
      {child},{finalRuns:[completed,{...completed,id:74}]}]){
      const pending=observe(changes);
      assert.equal(pending.run.status,0,pending.run.stderr);
      assert.equal(pending.fields.observation_outcome,'pending');
      assert.equal(pending.fields.verdict,undefined);
      assert.equal(pending.reading.product,null,'an unreported product is never measured zero');
      assert(pending.reading.pending>0);
    }
    for(const changes of [{child:{...completed,conclusion:'failure'}},
      {finalChild:{...completed,run_attempt:2}},{finalChild:{...completed,status:'in_progress',conclusion:null}}]){
      const refused=observe(changes);
      assert.equal(refused.run.status,0,refused.run.stderr);
      assert.equal(refused.fields.observation_outcome,'refused');
      assert.equal(refused.fields.verdict,undefined);
      assert.equal(refused.reading.product,null);
      assert.equal(refused.reading.refused,1);
    }
    for(const changes of [{missing:0},{partial:true}]){
      const unmeasured=observe(changes);
      assert.equal(unmeasured.run.status,1);
      assert.deepEqual(unmeasured.fields,{});
    }
    const foreignDecision=path.join(scratch,`${name}.json`);
    const foreignArchive=path.join(scratch,'foreign-product.zip');
    fs.writeFileSync(foreignDecision,JSON.stringify({...receipt,product:{...product,source_revision:'d'.repeat(40)}}));
    const foreignZip=spawnSync('zip',['-j',foreignArchive,foreignDecision],
      {env:{PATH:process.env.PATH,LC_ALL:'C'},encoding:'utf8'});
    assert.equal(foreignZip.status,0,foreignZip.stderr);
    const foreignBytes=fs.readFileSync(foreignArchive);
    const foreignReading=observe({archives:[...archives.slice(0,2),{archive:foreignArchive,metadata:{
      ...archives[2].metadata,size_in_bytes:foreignBytes.length,
      digest:`sha256:${createHash('sha256').update(foreignBytes).digest('hex')}`}}]});
    assert.equal(foreignReading.run.status,0,foreignReading.run.stderr);
    assert.equal(foreignReading.fields.observation_outcome,'refused');
    assert.equal(foreignReading.fields.verdict,undefined);
    assert.equal(foreignReading.reading.product,null);

    const failedFields=Object.fromEntries(failedProducer.output.trim().split('\n').map(line=>{
      const split=line.indexOf('=');return [line.slice(0,split),line.slice(split+1)];
    }));
    const failedArchive=path.join(scratch,'failed-product.zip');
    const failedZip=spawnSync('zip',['-j',failedArchive,failedFields.receipt],
      {env:{PATH:process.env.PATH,LC_ALL:'C'},encoding:'utf8'});
    assert.equal(failedZip.status,0,failedZip.stderr);
    const failedBytes=fs.readFileSync(failedArchive);
    const failedArchives=[...archives.slice(0,2),{archive:failedArchive,metadata:{
      ...archives[2].metadata,size_in_bytes:failedBytes.length,
      digest:`sha256:${createHash('sha256').update(failedBytes).digest('hex')}`}}];
    const reachedFailure=observe({archives:failedArchives,child:{...completed,conclusion:'failure'}});
    assert.equal(reachedFailure.run.status,0,reachedFailure.run.stderr);
    assert.equal(reachedFailure.fields.verdict,'fail');
    assert.deepEqual(reachedFailure.reading.product,failedProduct);
    assert.equal(reachedFailure.reading.execution,'failure');
    for(const changes of [{archives:failedArchives},{archives:failedArchives,child:{...completed,conclusion:'cancelled'}},
      {archives:failedArchives,child:{...completed,conclusion:'failure'},finalChild:{...completed,run_attempt:2,conclusion:'failure'}}]){
      const refused=observe(changes);assert.equal(refused.fields.observation_outcome,'refused');
      assert.equal(refused.fields.verdict,undefined);
      assert.equal(refused.reading.product,null);
    }
    const noProduct=observe({missing:2,child:{...completed,conclusion:'failure'}});
    assert.equal(noProduct.run.status,0,noProduct.run.stderr);
    assert.equal(noProduct.fields.observation_outcome,'execution_failed');
    assert.equal(noProduct.fields.verdict,undefined);
    assert.deepEqual(noProduct.reading,{discovered:1,owned:1,pending:0,refused:0,custody_commitment:commitment,execution:'failure',product:null});
    assert.equal(JSON.parse(fs.readFileSync(lastRead,'utf8')),'/repos/acme/widget-host/actions/runs/73');
    for(const changes of [{missing:2,child:{...completed,conclusion:'failure'},finalChild:{...completed,run_attempt:2,conclusion:'failure'}},
      {missing:2,child:{...completed,conclusion:'failure'},finalRuns:[completed,{...completed,id:74}]},
      {missing:2,child:{...completed,conclusion:'failure'},finalPartial:true}]){
      const unknown=observe(changes);assert.notEqual(unknown.fields.observation_outcome,'execution_failed');
      assert.equal(unknown.fields.verdict,undefined);
      assert.equal(unknown.reading.execution,null);assert.equal(unknown.reading.product,null);
    }
    const successWithoutProduct=observe({missing:2});
    assert.equal(successWithoutProduct.fields.observation_outcome,'pending');
    assert.equal(successWithoutProduct.fields.verdict,undefined);
    assert.equal(successWithoutProduct.reading.execution,'success');
    assert.equal(successWithoutProduct.reading.product,null);
    const waitPositive=observe({recordedClock:true},true);
    assert.equal(waitPositive.run.status,0,waitPositive.run.stderr);
    assert.equal(waitPositive.fields.verdict,'pass');
    const waitMissing=observe({recordedClock:true,missing:2},true);
    assert.equal(waitMissing.run.status,0,waitMissing.run.stderr);
    assert.equal(waitMissing.fields.observation_outcome,'pending');
    assert.equal(waitMissing.fields.wall_seconds,'2700');
    assert.equal(waitMissing.fields.verdict,undefined,'green execution without reached judgment cannot settle a release');
    const deadline=observe({recordedClock:true,clockStart:8200,missing:2},true);
    assert.equal(deadline.run.status,1,deadline.run.stderr);
    assert.equal(deadline.fields.observation_outcome,'deadline');
    assert.equal(deadline.fields.verdict,undefined);
    assert(deadline.fields.deadline_file,'reached non-null deadline must produce its closed receipt');
    const deadlineReceipt=JSON.parse(fs.readFileSync(deadline.fields.deadline_file,'utf8'));
    assert.deepEqual(deadlineReceipt,{schema:'harn.consumer_observer.deadline.v1',observer:context,observed_at:8200,
      reading:{discovered:1,owned:1,pending:1,refused:0,custody_commitment:commitment,execution:'success',product:null}});
    assert.equal(fs.statSync(deadline.fields.deadline_file).mode&0o777,0o600);
    const liveDeadline=observe({recordedClock:true,clockStart:8200,child},true);
    assert.equal(liveDeadline.fields.observation_outcome,'deadline');
    assert.equal(JSON.parse(fs.readFileSync(liveDeadline.fields.deadline_file,'utf8')).reading.custody_commitment,commitment);
    for(const change of [{finalParent:{...parent,status:'completed',conclusion:'failure'}},
      {finalSource:'d'.repeat(40)},{finalRuns:[child,{...child,id:74}]},
      {finalChild:{...child,run_attempt:2}},{finalChild:{...child,status:'completed',conclusion:'failure'}}]){
      const endedLive=observe({recordedClock:true,clockStart:8200,child,...change},true);
      assert.equal(endedLive.run.status,1);
      assert.equal(endedLive.fields.deadline_file,undefined,'live custody must be refreshed before minting a deadline');
    }
    for(const changes of [{runs:[]},{runs:[completed,{...completed,id:74}]},{missing:1},
      {child:{...completed,conclusion:'failure'}},{finalChild:{...completed,run_attempt:2}},
      {finalParent:{...parent,status:'completed',conclusion:'failure'}},{finalSource:'d'.repeat(40)}]){
      const unmeasuredDeadline=observe({recordedClock:true,clockStart:8200,missing:2,...changes},true);
      assert.equal(unmeasuredDeadline.run.status,1);
      assert.equal(unmeasuredDeadline.fields.deadline_file,undefined,'unmeasured/infrastructure evidence cannot mint deadline authority');
    }
    const forgedStart=observe({recordedClock:true,clockStart:8200,missing:2},true,
      {OBSERVER_CONTEXT:JSON.stringify({...context,started_at:900})});
    assert.equal(forgedStart.run.status,1);
    assert.equal(forgedStart.fields.deadline_file,undefined,'a backdated context cannot match the issued declaration');
    const lateProduct=observe({recordedClock:true,clockStart:8200},true);
    assert.equal(lateProduct.run.status,1);
    assert.equal(lateProduct.fields.deadline_file,undefined,'already reached product cannot become unmeasured deadline evidence');
    const waitExecutionFailure=observe({recordedClock:true,missing:2,child:{...completed,conclusion:'failure'}},true);
    assert.equal(waitExecutionFailure.run.status,1);
    assert.equal(waitExecutionFailure.fields.observation_outcome,'execution_failed');
    assert.equal(waitExecutionFailure.fields.verdict,undefined,'infrastructure failure cannot authorize product retirement');
    for(const changes of [{finalParent:{...parent,status:'completed',conclusion:'cancelled'}},
      {finalSource:'d'.repeat(40)}]){
      const staleWait=observe({recordedClock:true,missing:2,child:{...completed,conclusion:'failure'},...changes},true);
      assert.equal(staleWait.run.status,1);
      assert.equal(staleWait.fields.observation_outcome,'refused');
      assert.equal(staleWait.fields.verdict,undefined);
    }
    const pairedPull={state:'open',head:{repo:{full_name:'acme/widget-host'},ref:'paired-work',sha:'c'.repeat(40)}};
    const pairedChild={...completed,head_branch:'paired-work'};
    const pairedWait=observe({recordedClock:true,pull:pairedPull,child:pairedChild,runs:[pairedChild]},true,{CANARY_PAIRED_PULL:'7'});
    assert.equal(pairedWait.run.status,0,pairedWait.run.stderr);assert.equal(pairedWait.fields.verdict,'pass');
    for(const finalPull of [{...pairedPull,state:'closed'},
      {...pairedPull,head:{...pairedPull.head,sha:'d'.repeat(40)}},
      {...pairedPull,head:{...pairedPull.head,repo:{full_name:'other/fork'}}}]){
      const changedPair=observe({recordedClock:true,pull:pairedPull,finalPull,missing:2,
        child:{...pairedChild,conclusion:'failure'},runs:[pairedChild]},true,{CANARY_PAIRED_PULL:'7'});
      assert.equal(changedPair.run.status,1);assert.equal(changedPair.fields.observation_outcome,'refused');
      assert.equal(changedPair.fields.verdict,undefined);
    }

    const readParentEntry=fileURLToPath(new URL('../ci/consumer-observer-read-parent.mjs',import.meta.url));
    const readDecisionEntry=fileURLToPath(new URL('../ci/consumer-observer-read-decision.mjs',import.meta.url));
    const selectEntry=fileURLToPath(new URL('../ci/consumer-observer-select.mjs',import.meta.url));
    const terminalParent={...parent,status:'completed',conclusion:'success',created_at:'2026-10-11T20:00:00Z'};
    const reusableChild={...completed,head_branch:'main'};
    const readParent=(changes={},extraEnv={},select=false,decision=false,contextDecision=false,recovery=false,repair=false)=>{
      const output=path.join(scratch,`reuse-${sequence++}`);
      fs.writeFileSync(fixture,JSON.stringify({parent:terminalParent,child:reusableChild,runs:[reusableChild],archives,...changes}));
      const run=spawnSync(process.execPath,[contextDecision?readDecisionEntry:select?selectEntry:readParentEntry,
        ...(repair?['--repair']:recovery?['--recover']:decision?['--decision']:[])],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${observerPreload}`,GH_TOKEN:'fixture-read',PARENT_RUN_ID:'42',PARENT_ROLE:'canary',
        SOURCE_REVISION:context.source_revision,REQUIRE_CURRENT_DEFAULT:'true',CANARY_REPOSITORY:'acme/widget-host',
        CANARY_WORKFLOW:'rehearsal.yml',GITHUB_OUTPUT:output,REHEARSAL_PROMOTION_RUN_ID:'42',
        OBSERVER_CONTEXT:JSON.stringify(context),...extraEnv}});
      const text=fs.existsSync(output)?fs.readFileSync(output,'utf8'):'';
      assert.doesNotMatch(run.stdout+run.stderr+text,/widget-host|dispatch_actor|\/73|"run_id"/);
      return {run,text};
    };
    const reused=readParent();assert.equal(reused.run.status,0,reused.run.stderr);
    const reusedValue=JSON.parse(reused.run.stdout);
    assert.deepEqual(reusedValue.context,context);assert.deepEqual(reusedValue.reading.product,product);
    assert.equal(reusedValue.owned,false,'historical success does not grant new cancellation ownership');
    assert.match(reused.text,/owned=false/);
    assert.equal(JSON.parse(fs.readFileSync(lastRead,'utf8')),'/repos/acme/widget-host/actions/runs/73',
      'the fresh child status follows public-parent and current-default eligibility reads');
    const targets=JSON.parse(execFileSync('bash',[fileURLToPath(new URL('../lib/candidate_archive_contract.sh',import.meta.url)),'--targets-json'],{encoding:'utf8'}));
    const producerRun={id:900,run_attempt:1,head_sha:context.source_revision,head_branch:'main',event:'push',
      status:'completed',conclusion:'failure',path:'.github/workflows/build-release-binaries.yml',
      repository:{full_name:context.parent_repository},head_repository:{full_name:context.parent_repository}};
    const producerJobs=['Resolve release context','Prepare CLI AOT payload','Assemble candidate manifest','Release residual audit',
      'Consumer release rehearsal / Decide whether main moved',...targets.map(target=>`Build ${target}`),
      ...['linux','linux-arm64','windows','macos'].map(target=>`Release smoke / Release smoke (${target})`)]
      .map((name,index)=>({id:index+1,run_id:900,run_attempt:1,head_sha:context.source_revision,name,status:'completed',conclusion:'success'}));
    const producerArtifacts=[...targets.map(target=>`harn-${target}`),`candidate-manifest-${context.source_revision}`,'harn-release-files']
      .map((name,index)=>({id:index+800,name,expired:false,size_in_bytes:100,digest:`sha256:${'d'.repeat(64)}`,
        workflow_run:{id:900,head_sha:context.source_revision}}));
    const producerData={run:producerRun,jobs:producerJobs,artifacts:producerArtifacts};
    const repairRequest={schema:'harn.release.consumer_repair.v1',context,producer_run:'900',producer_attempt:1,consumer_pull:'7'};
    const repairInputs={parent:{...parent,path:'.github/workflows/promote-release.yml',event:'workflow_dispatch',
      head_branch:'main',conclusion:null,created_at:terminalParent.created_at},producerData,
      pull:pairedPull,child:pairedChild,runs:[pairedChild]};
    // Reach actual admission and terminal recording with the producing
    // manifest bound into custody, rather than borrowing a source-only pass.
    const admissionEntry=fileURLToPath(new URL('../ci/consumer-observer-admit.mjs',import.meta.url));
    const admissionEvent=path.join(scratch,'repair-event.json');
    const admitBound=(inputs={},changes={})=>{
      const directory=path.join(scratch,`repair-admission-${sequence++}`);fs.mkdirSync(directory);
      const output=path.join(directory,'outputs');
      fs.writeFileSync(admissionEvent,JSON.stringify({inputs:{source_revision:context.source_revision,
        observer:JSON.stringify(context),release_producer_run_id:'900',release_producer_run_attempt:'1',...inputs}}));
      fs.writeFileSync(fixture,JSON.stringify({...repairInputs,archives,child:{...pairedChild,status:'in_progress',conclusion:null},...changes}));
      const run=spawnSync(process.execPath,[admissionEntry],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${observerPreload}`,GH_TOKEN:'fixture-read',OBSERVER_CONTEXT:JSON.stringify(context),
        GITHUB_REPOSITORY:'acme/widget-host',GITHUB_RUN_ID:'73',GITHUB_RUN_ATTEMPT:'1',GITHUB_SHA:child.head_sha,
        GITHUB_WORKFLOW_REF:'acme/widget-host/.github/workflows/rehearsal.yml@refs/heads/paired-work',
        GITHUB_EVENT_PATH:admissionEvent,SOURCE_REVISION:context.source_revision,
        RELEASE_PRODUCER_RUN_ID:'900',RELEASE_PRODUCER_RUN_ATTEMPT:'1',RUNNER_TEMP:directory,GITHUB_OUTPUT:output}});
      return {run,text:fs.existsSync(output)?fs.readFileSync(output,'utf8'):''};
    };
    const boundAdmission=admitBound();assert.equal(boundAdmission.run.status,0,boundAdmission.run.stderr);
    const boundFile=/^receipt=(.+)$/m.exec(boundAdmission.text)?.[1];assert.ok(boundFile);
    const boundReceipt=JSON.parse(fs.readFileSync(boundFile,'utf8'));
    assert.equal(boundReceipt.schema,'harn.consumer_observer_receipt.v3');
    assert.equal(boundReceipt.producer.manifest_id,producerArtifacts.find(item=>item.name.startsWith('candidate-manifest-')).id);
    const wrongInput=admitBound({release_producer_run_id:'901'});
    assert.equal(wrongInput.run.status,1);assert.equal(wrongInput.text,'');
    for(const audit of [{status:'in_progress',conclusion:null},...['failure','cancelled','skipped','timed_out',null]
      .map(conclusion=>({status:'completed',conclusion}))]) {
      const refused=admitBound({},{producerData:{...producerData,jobs:producerJobs.map(job=>
        job.name==='Release residual audit'?{...job,...audit}:job)}});
      assert.equal(refused.run.status,1,'promotion admission cannot bind a pending or failed audit');
      assert.equal(refused.text,'');
    }
    const bindArchive=(file,slot)=>{
      const archive=path.join(scratch,`bound-${slot}.zip`);
      const zipped=spawnSync('zip',['-j',archive,file],{encoding:'utf8'});assert.equal(zipped.status,0,zipped.stderr);
      const bytes=fs.readFileSync(archive);
      return {archive,metadata:{...archives[slot].metadata,size_in_bytes:bytes.length,
        digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`}};
    };
    const boundArchives=[archives[0],bindArchive(boundFile,1),archives[2]];
    const boundTerminal=execute({parent:repairInputs.parent,archives:boundArchives});
    assert.equal(boundTerminal.run.status,0,boundTerminal.run.stderr);
    const boundTerminalFile=/^receipt=(.+)$/m.exec(boundTerminal.output)?.[1];assert.ok(boundTerminalFile);
    assert.equal(JSON.parse(fs.readFileSync(boundTerminalFile,'utf8')).schema,'harn.consumer_observer_terminal.v3');
    boundArchives[2]=bindArchive(boundTerminalFile,2);
    repairInputs.archives=boundArchives;
    const repair=(changes={},env={})=>readParent({...repairInputs,...changes},{
      REQUIRE_CURRENT_DEFAULT:'false',OBSERVER_CONTEXT:JSON.stringify(repairRequest),...env},false,false,true,false,true);
    const repaired=repair();assert.equal(repaired.run.status,0,repaired.run.stderr);
    const repairedValue=JSON.parse(repaired.run.stdout);
    assert.equal(repairedValue.schema,'harn.release.consumer_repair_decision.v1');
    assert.equal(repairedValue.reading.product.verdict,'pass');
    assert.equal(repairedValue.producer.producer_run,'900');
    assert.equal(repairedValue.owned,false);
    for(const changes of [{runs:[]},{runs:[pairedChild,{...pairedChild,id:74}]},{missing:0},{missing:1},{missing:2},{archives},
      {child:{...pairedChild,id:74},runs:[{...pairedChild,id:74}]},
      {finalPull:{...pairedPull,state:'closed'}},{finalPull:{...pairedPull,head:{...pairedPull.head,sha:'d'.repeat(40)}}},
      {producerData:{...producerData,jobs:producerJobs.slice(1)}},
      {producerData:{...producerData,artifacts:[]}},
      {producerData:{...producerData,artifacts:producerArtifacts.map(item=>item.name.startsWith('candidate-manifest-')?
        {...item,digest:`sha256:${'e'.repeat(64)}`}:item)}},
      {parent:{...repairInputs.parent,status:'completed',conclusion:'cancelled'}},
      {finalParent:{...repairInputs.parent,run_attempt:3}}]) {
      const refused=repair(changes);assert.equal(refused.run.status,1,refused.run.stdout);
      assert.equal(refused.text,'','unobserved, stale or unproved repair cannot emit publication selection');
    }
    // Recover only through the deadline receipt emitted by the actual waiter
    // above and the actual terminal product receipt emitted by this producer.
    const deadlineArchive=path.join(scratch,'archive-3.zip');
    const deadlineZip=spawnSync('zip',['-j',deadlineArchive,deadline.fields.deadline_file],
      {env:{PATH:process.env.PATH,LC_ALL:'C'},encoding:'utf8'});
    assert.equal(deadlineZip.status,0,deadlineZip.stderr);
    const deadlineBytes=fs.readFileSync(deadlineArchive);
    const recoveryArchives=[...archives,{archive:deadlineArchive,metadata:{id:504,
      name:observerDeadlineName(context),expired:false,size_in_bytes:deadlineBytes.length,
      digest:`sha256:${createHash('sha256').update(deadlineBytes).digest('hex')}`,
      workflow_run:{id:42,head_sha:parent.head_sha}}}];
    const timestamp=seconds=>new Date(seconds*1000).toISOString();
    const step=(name,number,start,end,conclusion)=>({name,number,status:'completed',conclusion,
      started_at:timestamp(start),completed_at:timestamp(end)});
    const parentJobs=[{id:701,run_id:42,run_attempt:2,head_sha:parent.head_sha,status:'completed',conclusion:'failure',steps:[
      step(observerDeclarationStep,2,999,1001,'success'),
      step(observerWindowSteps[2],8,8100,8201,'failure'),
      step(observerDeadlineStep,9,8201,8202,'success')]}];
    const laterJob={id:801,run_id:73,run_attempt:1,head_sha:child.head_sha,status:'completed',conclusion:'success',
      completed_at:timestamp(8203)};
    const recoveryInputs={parent:{...terminalParent,conclusion:'failure'},archives:recoveryArchives,parentJobs,childJobs:[laterJob]};
    const recover=(change={},env={})=>readParent({...recoveryInputs,...change},env,false,false,true,true);
    const recovered=recover();assert.equal(recovered.run.status,0,recovered.run.stderr);
    const recoveryValue=JSON.parse(recovered.run.stdout);
    assert.equal(recoveryValue.reading.product.verdict,'pass');
    assert.deepEqual(recoveryValue.deadline,deadlineReceipt);
    assert.equal(recoveryValue.owned,false,'later pass cannot acquire new cleanup ownership');
    assert.doesNotMatch(recovered.run.stdout,/completed_at|head_branch|"id"/,'private chronology stays private');
    assert.equal(JSON.parse(fs.readFileSync(lastRead,'utf8')),'/repos/acme/widget-host/actions/runs/73');
    for(const changes of [{childJobs:[]},{chronologyPartial:true},
      {childJobs:[{...laterJob,completed_at:timestamp(8199)}]},
      {childJobs:[{...laterJob,completed_at:timestamp(8200)}]},
      {childJobs:[{...laterJob,completed_at:null}]},
      {childJobs:[{...laterJob,conclusion:'skipped'}]},
      {childJobs:[{...laterJob,status:'in_progress',conclusion:null}]},
      {childJobs:[{...laterJob,run_attempt:2}]},{childJobs:[{...laterJob,run_id:74}]},
      {childJobs:[{...laterJob,head_sha:'d'.repeat(40)}]},
      {finalChild:{...reusableChild,run_attempt:2}},{finalSource:'d'.repeat(40)},
      {finalParent:{...terminalParent,run_attempt:3}},{missing:3},{missing:2}]){
      const refusedRecovery=recover(changes);
      assert.equal(refusedRecovery.run.status,1,refusedRecovery.run.stdout);
      assert.equal(refusedRecovery.text,'');
    }
    const falsePolicy=recover({},{REQUIRE_CURRENT_DEFAULT:'false'});
    assert.equal(falsePolicy.run.status,1);assert.equal(falsePolicy.text,'');
    // A separately admitted child remains valid ordinary custody even after
    // the original disappears. It must never replace the deadline's child.
    for(const [index,replacement,reuseNonce] of [
      [0,{...reusableChild,id:74},false],[1,{...reusableChild,id:74},true],
      [2,{...reusableChild,run_attempt:2},true],[3,{...reusableChild,head_sha:'d'.repeat(40)},true],
      [4,reusableChild,false],
    ]){
      const live={...replacement,status:'in_progress',conclusion:null};
      const replacementExpected={...expected,runId:String(live.id),attempt:live.run_attempt,headSha:live.head_sha};
      const replacementOwned=admitChild(context,parent,replacementExpected,live,
        {inputs:{source_revision:context.source_revision,observer:JSON.stringify(context)}},context);
      if(reuseNonce)replacementOwned.child.receipt_nonce=ownership.child.receipt_nonce;
      const replacementTerminal=createTerminalReceipt(replacementOwned,context,parent,live,
        replacementExpected,context,product);
      const changedArchives=[...recoveryArchives];
      for(const [slot,value] of [[1,replacementOwned],[2,replacementTerminal]]){
        const json=path.join(scratch,`replacement-${index}-${slot}`,`${name}.json`);
        fs.mkdirSync(path.dirname(json));fs.writeFileSync(json,JSON.stringify(value));
        const zip=path.join(scratch,`replacement-${index}-${slot}.zip`);
        const zipped=spawnSync('zip',['-j',zip,json],{env:{PATH:process.env.PATH,LC_ALL:'C'},encoding:'utf8'});
        assert.equal(zipped.status,0,zipped.stderr);
        const bytes=fs.readFileSync(zip);
        changedArchives[slot]={archive:zip,metadata:{...recoveryArchives[slot].metadata,
          size_in_bytes:bytes.length,digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,
          workflow_run:{id:replacement.id,head_sha:replacement.head_sha}}};
      }
      const change={archives:changedArchives,child:replacement,runs:[replacement],source:replacement.head_sha,
        childJobs:[{...laterJob,run_id:replacement.id,run_attempt:replacement.run_attempt,head_sha:replacement.head_sha}]};
      const ordinary=readParent({...recoveryInputs,...change},{},false,false,true);
      assert.equal(ordinary.run.status,0,ordinary.run.stderr);
      assert.equal(JSON.parse(ordinary.run.stdout).reading.product.verdict,'pass');
      if(index===0){
        // The original was authenticated first, but disappears from the final
        // complete census. Exactly one valid replacement is not original custody.
        const replacedAtDeadline=observe({recordedClock:true,clockStart:8200,child,finalRuns:[live]},true);
        assert.equal(replacedAtDeadline.run.status,1);
        assert.equal(replacedAtDeadline.fields.deadline_file,undefined);
        const replacedReading=observe({child,finalRuns:[live]}).reading;
        assert.equal(replacedReading.discovered,1);
        assert.equal(replacedReading.owned,0);
        assert.equal(replacedReading.custody_commitment,null);
        assert.equal(replacedReading.pending,1);
        assert.equal(replacedReading.product,null);
      }
      const swapped=recover(change);assert.equal(swapped.run.status,1,swapped.run.stdout);
      assert.equal(swapped.text,'');
      assert(!(ordinary.run.stdout+ordinary.run.stderr+ordinary.text).includes(replacementOwned.child.receipt_nonce));
      assert(!(swapped.run.stdout+swapped.run.stderr).includes(replacementOwned.child.receipt_nonce));
    }
    const reachedHistoricalFailure=readParent({parent:{...terminalParent,conclusion:'failure'},
      archives:failedArchives,child:{...reusableChild,conclusion:'failure'}},{},false,true);
    assert.equal(reachedHistoricalFailure.run.status,0,reachedHistoricalFailure.run.stderr);
    const historicalFailure=JSON.parse(reachedHistoricalFailure.run.stdout);
    assert.deepEqual(historicalFailure.reading.product,failedProduct);
    assert.equal(historicalFailure.owned,false);
    assert.equal(historicalFailure.reading.execution,'failure');
    for(const changes of [{missing:2,child:{...reusableChild,conclusion:'failure'}},
      {archives:failedArchives,child:{...reusableChild,conclusion:'failure'},finalChild:{...reusableChild,run_attempt:2,conclusion:'failure'}},
      {archives:failedArchives,child:{...reusableChild,conclusion:'failure'},finalParent:{...terminalParent,run_attempt:3}},
      {archives:failedArchives,child:{...reusableChild,conclusion:'failure'},finalSource:'d'.repeat(40)}]){
      const unknownDecision=readParent(changes,{},false,true);
      assert.equal(unknownDecision.run.status,1);assert.equal(unknownDecision.text,'');
    }
    for(const selectedParent of [terminalParent,{...terminalParent,status:'in_progress',conclusion:null}]){
      const contextPass=readParent({parent:selectedParent},{},false,false,true);
      assert.equal(contextPass.run.status,0,contextPass.run.stderr);
      assert.equal(JSON.parse(contextPass.run.stdout).reading.product.verdict,'pass');
      assert.equal(JSON.parse(contextPass.run.stdout).owned,false);
      const contextFail=readParent({parent:selectedParent,archives:failedArchives,
        child:{...reusableChild,conclusion:'failure'}},{},false,false,true);
      assert.equal(contextFail.run.status,0,contextFail.run.stderr);
      assert.deepEqual(JSON.parse(contextFail.run.stdout).reading.product,failedProduct);
    }
    for(const changes of [{missing:2},{missing:2,child:{...reusableChild,conclusion:'failure'}},
      {finalParent:{...terminalParent,run_attempt:3}},
      {parent:{...terminalParent,status:'in_progress',conclusion:null},finalParent:terminalParent},
      {finalSource:'d'.repeat(40)},
      {finalChild:{...reusableChild,run_attempt:2}},
      {finalRuns:[reusableChild,{...reusableChild,id:74}]},{finalPartial:true}]){
      const contextUnknown=readParent(changes,{},false,false,true);
      assert.equal(contextUnknown.run.status,1);assert.equal(contextUnknown.text,'');
    }
    for(const extraEnv of [{SOURCE_REVISION:'d'.repeat(40)},{REQUIRE_CURRENT_DEFAULT:'unknown'},
      {OBSERVER_CONTEXT:JSON.stringify({...context,dispatch_nonce:'11234567-89ab-4cde-8fab-0123456789ab'})}]){
      const contextUnknown=readParent({},extraEnv,false,false,true);
      assert.equal(contextUnknown.run.status,1);assert.equal(contextUnknown.text,'');
    }
    for(const changes of [{runs:[]},{runs:[reusableChild,{...reusableChild,id:74}]},{missing:0},{missing:1},{missing:2},
      {partial:true},{publication:'skipped'},{publication:'failure'},
      {parent:{...terminalParent,status:'in_progress',conclusion:null}},
      {finalParent:{...terminalParent,run_attempt:3}},
      {child:{...reusableChild,head_branch:'other'}},{source:'d'.repeat(40)},{finalSource:'d'.repeat(40)},
      {archives:failedArchives,child:{...reusableChild,conclusion:'failure'}},
      {child:{...reusableChild,conclusion:'failure'}},{finalChild:{...reusableChild,run_attempt:2}}]){
      const negative=readParent(changes);assert.equal(negative.run.status,1,JSON.stringify(changes));assert.equal(negative.text,'');
    }
    for(const extraEnv of [{PARENT_ROLE:'promotion'},{PARENT_ROLE:'unknown'},
      {SOURCE_REVISION:'d'.repeat(40)},{REQUIRE_CURRENT_DEFAULT:'unknown'}]){
      const negative=readParent({},extraEnv);assert.equal(negative.run.status,1);assert.equal(negative.text,'');
    }
    for(const changes of [{finalParent:{...terminalParent,run_attempt:3}},
      {finalSource:'d'.repeat(40)}]){
      const missing=readParent({missing:2,child:{...reusableChild,conclusion:'failure'},...changes});
      assert.equal(missing.run.status,1);assert.equal(missing.text,'');
      // Reuse still refreshes parent/default eligibility when no product
      // artifact exists, rather than returning from an older child snapshot.
      assert.notEqual(JSON.parse(fs.readFileSync(lastRead,'utf8')),'/repos/acme/widget-host/actions/runs/73');
    }
    const promotion={...terminalParent,path:'.github/workflows/promote-release.yml'};
    const selected=readParent({parent:promotion},{},true);
    assert.equal(selected.run.status,0,selected.run.stderr);
    assert.match(selected.text,/reused=true\nverdict=pass\n/);
    assert.match(selected.run.stdout,/owned=false/);
    for(const changes of [{parent:terminalParent},{parent:promotion,missing:2},
      {parent:promotion,source:'d'.repeat(40)}]){
      const unknown=readParent(changes,{},true);
      assert.equal(unknown.run.status,1);assert.equal(unknown.text,'');
    }
    const fresh=readParent({}, {REHEARSAL_PROMOTION_RUN_ID:'',PAIRING_TEXT:'Pairs-with: consumer#7'},true);
    assert.equal(fresh.run.status,0,fresh.run.stderr);assert.equal(fresh.text,'reused=false\npaired_pull=7\n');
    const freshDefault=readParent({}, {REHEARSAL_PROMOTION_RUN_ID:'',PAIRING_TEXT:'no pairing trailer'},true);
    assert.equal(freshDefault.run.status,0,freshDefault.run.stderr);
    assert.equal(freshDefault.text,'reused=false\npaired_pull=\n');
    for(const pairing of ['Pairs-with: consumer#7\nPairs-with: consumer#8','Pairs-with: other#7',
      'Pairs-with: consumer#7\nPairs-with: other#7','Pairs-with: consumer#0']){
      const invalid=readParent({}, {REHEARSAL_PROMOTION_RUN_ID:'',PAIRING_TEXT:pairing},true);
      assert.equal(invalid.run.status,1);assert.equal(invalid.text,'');
    }
    const pairedReuse=readParent({parent:promotion},{PAIRING_TEXT:'Pairs-with: consumer#7'},true);
    assert.equal(pairedReuse.run.status,1);assert.equal(pairedReuse.text,'');
  } finally {
    fs.rmSync(scratch,{recursive:true});
  }
});
