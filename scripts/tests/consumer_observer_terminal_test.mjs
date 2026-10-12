import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {observerDeclarationName, observerDeclarationStep, observerDispatchActor, observerRunName, observerTerminalName} from '../lib/consumer-observer.mjs';

test('the terminal entrypoint records only an authenticated reached product decision', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-terminal-'));
  try {
    const context = {schema:'harn.consumer_observer.v1',parent_repository:'burin-labs/harn',
      parent_run_id:'42',parent_run_attempt:2,parent_head_sha:'a'.repeat(40),
      source_revision:'b'.repeat(40),dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab'};
    const parent = {id:42,run_attempt:2,head_sha:context.parent_head_sha,
      head_repository:{full_name:context.parent_repository},path:'.github/workflows/consumer-canary.yml',
      status:'in_progress'};
    const child = {id:73,run_attempt:1,head_sha:'c'.repeat(40),head_repository:{full_name:'acme/widget-host'},
      path:'.github/workflows/rehearsal.yml',event:'workflow_dispatch',status:'in_progress',
      actor:{login:observerDispatchActor,type:'Bot'},display_title:observerRunName(context)};
    const ownership = {schema:'harn.consumer_observer_receipt.v1',observer:context,child:{
      repository:'acme/widget-host',workflow:child.path,run_id:'73',run_attempt:1,
      head_sha:child.head_sha,dispatch_actor:observerDispatchActor}};
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
    assert.equal(receipt.schema,'harn.consumer_observer_terminal.v1');
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
      let censusReads=0,childReads=0,parentReads=0,sourceReads=0;
      globalThis.fetch=async(url,options={})=>{
        if(options.method==='POST')throw new Error('unexpected-private-post');
        const endpoint=new URL(url),p=endpoint.pathname;
        fs.writeFileSync(${JSON.stringify(lastRead)},JSON.stringify(p));
        if(p.endsWith('/zip')){
          const index=Number(/artifacts\\/(50[123])\\//.exec(p)?.[1])-501;
          return {ok:true,arrayBuffer:async()=>fs.readFileSync(fixture.archives[index].archive)};
        }
        let record;
        if(p.endsWith('/artifacts')){
          const wanted=endpoint.searchParams.get('name');
          if(!wanted&&!p.includes('/42/'))throw new Error('unfiltered-private-read');
          const selected=fixture.archives.filter((a,index)=>(wanted?a.metadata.name===wanted:index===0)&&fixture.missing!==index);
          record={total_count:selected.length,artifacts:selected.map(a=>a.metadata)};
        }else if(p.endsWith('/42/attempts/2'))record=fixture.parent;
        else if(p.endsWith('/42/attempts/2/jobs'))record={total_count:1,jobs:[{id:701,conclusion:'success',steps:[{
          name:${JSON.stringify(observerDeclarationStep)},status:'completed',conclusion:fixture.publication??'success'}]}]};
        else if(p.endsWith('/42'))record=++parentReads>1&&fixture.finalParent?fixture.finalParent:fixture.parent;
        else if(p==='/repos/acme/widget-host')record={full_name:'acme/widget-host',default_branch:'main'};
        else if(p.endsWith('/commits/main'))record={sha:++sourceReads>1&&fixture.finalSource?fixture.finalSource:fixture.source??'c'.repeat(40)};
        else if(p.endsWith('/73'))record=++childReads>1&&fixture.finalChild?fixture.finalChild:fixture.child;
        else if(p.endsWith('/rehearsal.yml/runs')){
          const rows=++censusReads>1&&fixture.finalRuns?fixture.finalRuns:fixture.runs;
          record={total_count:fixture.partial?rows.length+1:rows.length,workflow_runs:rows};
        }else throw new Error('unknown-private-url');
        return {ok:true,json:async()=>record,headers:new Headers()};
      };`);
    const completed={...child,status:'completed',conclusion:'success'};
    const observeEntry=fileURLToPath(new URL('../ci/consumer-observer-observe.mjs',import.meta.url));
    const observe=(changes={})=>{
      const output=path.join(scratch,`observation-${sequence++}`);
      fs.writeFileSync(fixture,JSON.stringify({parent:{...parent,created_at:'2026-10-11T20:00:00Z'},
        child:completed,runs:[completed],archives,...changes}));
      const run=spawnSync(process.execPath,[observeEntry],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${observerPreload}`,GH_TOKEN:'fixture-read',OBSERVER_CONTEXT:JSON.stringify(context),
        CANARY_OWNER:'acme',CANARY_NAME:'widget-host',CANARY_WORKFLOW:'rehearsal.yml',GITHUB_OUTPUT:output}});
      const text=fs.existsSync(output)?fs.readFileSync(output,'utf8'):'';
      assert.doesNotMatch(run.stdout+run.stderr+text,/widget-host|run_id|head_sha|dispatch_actor|\/73/);
      const fields=Object.fromEntries(text.trim().split('\n').filter(Boolean).map(line=>{
        const split=line.indexOf('=');return [line.slice(0,split),line.slice(split+1)];
      }));
      return {run,fields,reading:fields.reading?JSON.parse(fields.reading):null};
    };
    const measured=observe();
    assert.equal(measured.run.status,0,measured.run.stderr);
    assert.equal(measured.fields.verdict,'pass');
    assert.deepEqual(measured.reading,{discovered:1,owned:1,pending:0,refused:0,product});
    for(const changes of [{runs:[]},{runs:[completed,{...completed,id:74}]},{missing:1},{missing:2},
      {child},{finalRuns:[completed,{...completed,id:74}]}]){
      const pending=observe(changes);
      assert.equal(pending.run.status,0,pending.run.stderr);
      assert.equal(pending.fields.verdict,'pending');
      assert.equal(pending.reading.product,null,'an unreported product is never measured zero');
      assert(pending.reading.pending>0);
    }
    for(const changes of [{child:{...completed,conclusion:'failure'}},
      {finalChild:{...completed,run_attempt:2}},{finalChild:{...completed,status:'in_progress',conclusion:null}}]){
      const refused=observe(changes);
      assert.equal(refused.run.status,0,refused.run.stderr);
      assert.equal(refused.fields.verdict,'refused');
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
    assert.equal(foreignReading.fields.verdict,'refused');
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
    for(const changes of [{archives:failedArchives},{archives:failedArchives,child:{...completed,conclusion:'cancelled'}},
      {archives:failedArchives,child:{...completed,conclusion:'failure'},finalChild:{...completed,run_attempt:2,conclusion:'failure'}}]){
      const refused=observe(changes);assert.equal(refused.fields.verdict,'refused');
      assert.equal(refused.reading.product,null);
    }

    const readParentEntry=fileURLToPath(new URL('../ci/consumer-observer-read-parent.mjs',import.meta.url));
    const terminalParent={...parent,status:'completed',conclusion:'success',created_at:'2026-10-11T20:00:00Z'};
    const reusableChild={...completed,head_branch:'main'};
    const readParent=(changes={},extraEnv={})=>{
      const output=path.join(scratch,`reuse-${sequence++}`);
      fs.writeFileSync(fixture,JSON.stringify({parent:terminalParent,child:reusableChild,runs:[reusableChild],archives,...changes}));
      const run=spawnSync(process.execPath,[readParentEntry],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${observerPreload}`,GH_TOKEN:'fixture-read',PARENT_RUN_ID:'42',PARENT_ROLE:'canary',
        SOURCE_REVISION:context.source_revision,REQUIRE_CURRENT_DEFAULT:'true',CANARY_REPOSITORY:'acme/widget-host',
        CANARY_WORKFLOW:'rehearsal.yml',GITHUB_OUTPUT:output,...extraEnv}});
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
  } finally {
    fs.rmSync(scratch,{recursive:true});
  }
});
