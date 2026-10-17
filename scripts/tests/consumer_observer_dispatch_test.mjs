import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {observerDeclarationName} from '../lib/consumer-observer.mjs';

test('the actual dispatch entrypoint requires issued custody and a fresh live parent, and exports no child identity',()=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-observer-dispatch-'));
  try {
    const context={schema:'harn.consumer_observer.v2',parent_repository:'burin-labs/harn',
      parent_run_id:'42',parent_run_attempt:2,parent_head_sha:'a'.repeat(40),
      source_revision:'b'.repeat(40),dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab',started_at:1000};
    const parent={id:42,run_attempt:2,head_sha:context.parent_head_sha,
      head_repository:{full_name:context.parent_repository},path:'.github/workflows/consumer-canary.yml',status:'in_progress'};
    const declaration=path.join(scratch,'consumer-observer.json'),archive=path.join(scratch,'declaration.zip');
    fs.writeFileSync(declaration,JSON.stringify(context));
    const zipped=spawnSync('zip',['-j',archive,declaration],{env:{PATH:process.env.PATH,LC_ALL:'C'},encoding:'utf8'});
    assert.equal(zipped.status,0,zipped.stderr);
    const bytes=fs.readFileSync(archive);
    const metadata={id:501,name:observerDeclarationName(context),expired:false,size_in_bytes:bytes.length,
      digest:`sha256:${createHash('sha256').update(bytes).digest('hex')}`,workflow_run:{id:42,head_sha:parent.head_sha}};
    const fixture=path.join(scratch,'fixture.json'),calls=path.join(scratch,'calls.jsonl'),preload=path.join(scratch,'fetch.mjs');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const f=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
      globalThis.fetch=async(url,options={})=>{
        if(options.headers?.Authorization!==((options.method==='POST'||url.includes('/repos/acme/'))?'Bearer fixture-consumer':'Bearer fixture-parent'))throw new Error('authority-scope-mismatch');
        fs.appendFileSync(${JSON.stringify(calls)},JSON.stringify({url,method:options.method??'GET',body:options.body})+String.fromCharCode(10));
        const endpoint=new URL(url),p=endpoint.pathname;
        if(options.method==='POST'){
          if(p!='/repos/acme/widget-host/actions/workflows/rehearsal.yml/dispatches')throw new Error('unexpected-dispatch');
          return {status:f.dispatchStatus??204};
        }
        if(p.endsWith('/artifacts')){
          if(endpoint.searchParams.get('name')!==f.metadata.name)throw new Error('unqualified-census');
          return {ok:true,json:async()=>({total_count:f.missing?0:1,artifacts:f.missing?[]:[f.metadata]})};
        }
        if(p.endsWith('/501/zip'))return {ok:true,arrayBuffer:async()=>fs.readFileSync(${JSON.stringify(archive)})};
        if(p.endsWith('/42'))return {ok:true,json:async()=>f.parent};
        if(p==='/repos/acme/widget-host')return {ok:true,json:async()=>({full_name:'acme/widget-host',default_branch:f.branch??'main'})};
        if(p.endsWith('/commits/main'))return {ok:true,json:async()=>({sha:'c'.repeat(40)})};
        if(p.endsWith('/pulls/7'))return {ok:true,json:async()=>f.pull};
        throw new Error('unexpected-private-child-read');
      };`);
    const entry=fileURLToPath(new URL('../ci/consumer-observer-dispatch.mjs',import.meta.url));
    const execute=(change={},envChange={})=>{
      fs.writeFileSync(fixture,JSON.stringify({metadata,parent,...change}));fs.writeFileSync(calls,'');
      const run=spawnSync(process.execPath,[entry],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${preload}`,GH_TOKEN:'fixture-consumer',PARENT_READ_TOKEN:'fixture-parent',OBSERVER_CONTEXT:JSON.stringify(context),
        GITHUB_REPOSITORY:context.parent_repository,GITHUB_RUN_ID:context.parent_run_id,
        GITHUB_RUN_ATTEMPT:String(context.parent_run_attempt),GITHUB_SHA:context.parent_head_sha,
        CANARY_REPOSITORY:'acme/widget-host',CANARY_WORKFLOW:'rehearsal.yml',CANARY_TARGET:'v0.10.165',...envChange}});
      assert.doesNotMatch(run.stdout+run.stderr,/widget-host|paired-work|run_id|actions\/runs|fixture/);
      return {run,calls:fs.readFileSync(calls,'utf8').trim().split(String.fromCharCode(10)).filter(Boolean).map(JSON.parse)};
    };
    const good=execute();
    assert.equal(good.run.status,0,good.run.stderr);
    assert.match(good.run.stdout,/dispatched=true child_identity=private/);
    const post=good.calls.filter(c=>c.method==='POST');assert.equal(post.length,1);
    const dispatched=JSON.parse(post[0].body);
    assert.deepEqual(JSON.parse(dispatched.inputs.observer),context);
    assert.deepEqual({...dispatched,inputs:{...dispatched.inputs,observer:context}},
      {ref:'main',inputs:{target:'v0.10.165',source_revision:context.source_revision,legs:'clean',observer:context}});
    const forgedRef=execute({},{CANARY_REF:'unreviewed-ref'});
    assert.equal(forgedRef.run.status,0);
    assert.equal(JSON.parse(forgedRef.calls.find(c=>c.method==='POST').body).ref,'main');
    const paired={state:'open',head:{repo:{full_name:'acme/widget-host'},ref:'paired-work',sha:'c'.repeat(40)}};
    const pairedDispatch=execute({pull:paired},{CANARY_PAIRED_PULL:'7'});
    assert.equal(pairedDispatch.run.status,0,pairedDispatch.run.stderr);
    assert.equal(JSON.parse(pairedDispatch.calls.find(c=>c.method==='POST').body).ref,'paired-work');
    for(const pull of [{...paired,state:'closed'},
      {...paired,head:{...paired.head,repo:{full_name:'foreign/fork'}}},
      {...paired,head:{...paired.head,sha:'unknown'}}]){
      const badPair=execute({pull},{CANARY_PAIRED_PULL:'7'});
      assert.equal(badPair.run.status,1);assert.equal(badPair.calls.filter(c=>c.method==='POST').length,0);
    }
    for(const change of [{missing:true},{parent:{...parent,status:'completed',conclusion:'success'}},
      {parent:{...parent,run_attempt:3}},{parent:{...parent,head_sha:'c'.repeat(40)}},
      {parent:{...parent,path:'.github/workflows/untrusted.yml'}}]){
      const bad=execute(change);assert.equal(bad.run.status,1);assert.equal(bad.calls.filter(c=>c.method==='POST').length,0);
    }
    for(const envChange of [{CANARY_TARGET:'dev'}, {CANARY_WORKFLOW:'../rehearsal.yml'},
      {CANARY_PAIRED_PULL:'bad'},{CANARY_REPOSITORY:'acme/'},{CANARY_REPOSITORY:'acme/widget-host?foreign=1'},
      {GITHUB_RUN_ID:'43'},{GITHUB_RUN_ATTEMPT:'3'},{GITHUB_SHA:'d'.repeat(40)},
      {GITHUB_REPOSITORY:'acme/widget-host'},{GH_TOKEN:''},{PARENT_READ_TOKEN:''},
      {OBSERVER_CONTEXT:JSON.stringify({...context,dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ac'})}]){
      const bad=execute({},envChange);assert.equal(bad.run.status,1);assert.equal(bad.calls.filter(c=>c.method==='POST').length,0);
    }
    const refused=execute({dispatchStatus:422});assert.equal(refused.run.status,1);
    assert.match(refused.run.stderr,/reason=dispatch_refused/);
    const bin=path.join(scratch,'bin'),ghCalls=path.join(scratch,'gh-calls.jsonl');
    fs.mkdirSync(bin);
    fs.writeFileSync(path.join(bin,'gh'),[
      '#!/usr/bin/env node',
      "const fs=require('node:fs');",
      "const args=process.argv.slice(2);fs.appendFileSync(process.env.GH_CALLS,JSON.stringify(args)+'\\n');",
      "if(args[0]==='workflow'&&args[1]==='run')console.log('https://github.com/acme/widget-host/actions/runs/73');",
      "else if(args[0]==='api'&&args[1]==='repos/acme/widget-host')console.log('main');",
      "else process.exitCode=1;",
    ].join('\n'),{mode:0o700});
    const bashEntry=fileURLToPath(new URL('../ci/consumer_canary.sh',import.meta.url));
    const bashDispatch=(change={},envChange={})=>{
      fs.writeFileSync(fixture,JSON.stringify({metadata,parent,...change}));
      fs.writeFileSync(calls,'');fs.writeFileSync(ghCalls,'');
      const run=spawnSync('bash',[bashEntry,'--dispatch'],{encoding:'utf8',env:{
        PATH:bin+path.delimiter+process.env.PATH,NODE_OPTIONS:'--import='+preload,GH_CALLS:ghCalls,
        GITHUB_OUTPUT:path.join(scratch,'bash-output'),
        GH_TOKEN:'fixture-consumer',PARENT_READ_TOKEN:'fixture-parent',OBSERVER_CONTEXT:JSON.stringify(context),
        GITHUB_REPOSITORY:context.parent_repository,GITHUB_RUN_ID:context.parent_run_id,
        GITHUB_RUN_ATTEMPT:String(context.parent_run_attempt),GITHUB_SHA:context.parent_head_sha,
        CANARY_REPOSITORY:'acme/widget-host',CANARY_WORKFLOW:'rehearsal.yml',
        SOURCE_REVISION:context.source_revision,WORKSPACE_VERSION:'1.2.4-dev',...envChange,
      }});
      assert.doesNotMatch(run.stdout+run.stderr,/widget-host|fixture-parent|fixture-consumer|dispatch_nonce/);
      return {run,calls:fs.readFileSync(calls,'utf8').trim().split(String.fromCharCode(10)).filter(Boolean).map(JSON.parse),
        gh:fs.readFileSync(ghCalls,'utf8').trim().split(String.fromCharCode(10)).filter(Boolean).map(JSON.parse)};
    };
    const bashGood=bashDispatch();
    assert.equal(bashGood.run.status,0,bashGood.run.stdout+bashGood.run.stderr);
    assert.match(bashGood.run.stdout,/started_at=1000/,'declaration qualification cannot reset the original clock');
    const dispatchedArgs=bashGood.gh.filter(args=>args[0]==='workflow');
    assert.equal(dispatchedArgs.length,1,'one actual Bash dispatch reached after canonical Node admission');
    assert.deepEqual(JSON.parse(dispatchedArgs[0].find(arg=>arg.startsWith('observer=')).slice(9)),context);
    assert(bashGood.calls.some(call=>call.url.includes('/501/zip')),'the actual Node owner authenticated the public declaration');
    assert.equal(bashGood.calls.filter(call=>call.method==='POST').length,0,'admission does not duplicate the Bash dispatch');
    for(const change of [{missing:true},{parent:{...parent,status:'completed'}},{parent:{...parent,run_attempt:3}}]){
      const blocked=bashDispatch(change);
      assert.equal(blocked.run.status,1);
      assert.equal(blocked.gh.filter(args=>args[0]==='workflow').length,0,'unproven custody blocks the real Bash dispatch');
    }
    for(const envChange of [{OBSERVER_CONTEXT:''},{PARENT_READ_TOKEN:''},
      {SOURCE_REVISION:'d'.repeat(40)},{GITHUB_RUN_ID:'43'}]){
      const blocked=bashDispatch({},envChange);
      assert.equal(blocked.run.status,1);
      assert.equal(blocked.gh.filter(args=>args[0]==='workflow').length,0);
    }
  } finally { fs.rmSync(scratch,{recursive:true}); }
});
