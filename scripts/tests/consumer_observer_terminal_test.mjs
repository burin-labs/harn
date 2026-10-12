import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {observerDeclarationName, observerDispatchActor, observerRunName} from '../lib/consumer-observer.mjs';

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
    const product = {source_revision:context.source_revision,verdict:'pass',owed:2,proven:2,pending:0,failed:0};
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
    const receipt=JSON.parse(fs.readFileSync(recorded.receipt,'utf8'));
    assert.equal(receipt.schema,'harn.consumer_observer_terminal.v1');
    assert.deepEqual(receipt.product,product);
    assert.deepEqual(receipt.child,ownership.child);
    assert.equal(fs.statSync(recorded.receipt).mode&0o777,0o600);
    for(const decision of [null,{}, {...product,owed:0,proven:0}, {...product,proven:1},
      {...product,pending:1}, {...product,failed:1}, {...product,source_revision:'d'.repeat(40)},
      {...product,extra:'unmeasured'}]){
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
  } finally {
    fs.rmSync(scratch,{recursive:true});
  }
});
