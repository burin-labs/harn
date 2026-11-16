import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import test from 'node:test';
import {observerDeclarationName} from '../lib/consumer-observer.mjs';
import {archiveJson} from './fixtures/artifact_archive.mjs';
import {compositionFixture,repository,declaration} from './fixtures/consumer_composition.mjs';

for(const scenario of ['measured','missing-component','recovery']){
  test(`actual composed Bash dispatch ${scenario}`,t=>{
    const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-composition-dispatch-'));
    t.after(()=>fs.rmSync(scratch,{recursive:true,force:true}));
    const fixture=compositionFixture();
    if(scenario==='missing-component')fixture.records.delete('/pulls/8');
    const context={schema:'harn.consumer_observer.v2',parent_repository:'burin-labs/harn',parent_run_id:'42',
      parent_run_attempt:1,parent_head_sha:'e'.repeat(40),source_revision:'f'.repeat(40),
      dispatch_nonce:'01234567-89ab-4cde-8fab-0123456789ab',started_at:1000};
    const parent={id:42,run_attempt:1,head_sha:context.parent_head_sha,
      head_repository:{full_name:context.parent_repository},path:'.github/workflows/consumer-canary.yml',status:'in_progress'};
    const archive=archiveJson(scratch,'consumer-observer.json',context,
      {id:501,name:observerDeclarationName(context),workflow_run:{id:42,head_sha:parent.head_sha}});
    const records=path.join(scratch,'records.json'),preload=path.join(scratch,'fetch.mjs');
    const reads=path.join(scratch,'reads.jsonl'),dispatches=path.join(scratch,'dispatches.jsonl'),output=path.join(scratch,'output');
    fs.writeFileSync(records,JSON.stringify([...fixture.records]),{mode:0o600});
    fs.writeFileSync(reads,'');fs.writeFileSync(dispatches,'');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      import {readRetry} from '${new URL('../lib/consumer-observer.mjs',import.meta.url).href}';
      readRetry.backoffMs=0;
      const records=new Map(JSON.parse(fs.readFileSync(process.env.FIXTURE_RECORDS,'utf8')));
      globalThis.fetch=async(url,options)=>{
        const endpoint=new URL(url);
        fs.appendFileSync(process.env.FIXTURE_READS,endpoint.pathname+endpoint.search+'\\n');
        if(endpoint.pathname.startsWith('/repos/${repository}')){
          if(options.headers.Authorization!=='Bearer consumer-read')throw new Error('wrong consumer authority');
          const value=records.get(endpoint.pathname.replace('/repos/${repository}','')+endpoint.search);
          return new Response(JSON.stringify(value??{}),{status:value?200:404});
        }
        if(options.headers.Authorization!=='Bearer parent-read')throw new Error('wrong parent authority');
        if(endpoint.pathname.endsWith('/artifacts'))return new Response(JSON.stringify({total_count:1,artifacts:[${JSON.stringify(archive.metadata)}]}));
        if(endpoint.pathname.endsWith('/501/zip'))return new Response(fs.readFileSync(${JSON.stringify(archive.archive)}));
        if(endpoint.pathname.endsWith('/runs/42'))return new Response(JSON.stringify(${JSON.stringify(parent)}));
        throw new Error('unexpected authenticated read');
      };`,{mode:0o600});
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
      import fs from 'node:fs';
      const args=process.argv.slice(2);
      if(args[0]!=='workflow'||args[1]!=='run')process.exit(1);
      fs.appendFileSync(process.env.FIXTURE_DISPATCHES,JSON.stringify(args)+'\\n');
      console.log('https://github.com/${repository}/actions/runs/73');`,{mode:0o700});
    const entry=fileURLToPath(new URL('../ci/consumer_canary.sh',import.meta.url));
    const run=spawnSync('bash',[entry,'--dispatch'],{encoding:'utf8',env:{
      PATH:`${scratch}:${process.env.PATH}`,NODE_OPTIONS:`--import=${preload}`,GH_TOKEN:'consumer-read',
      PARENT_READ_TOKEN:'parent-read',OBSERVER_CONTEXT:JSON.stringify(context),
      GITHUB_REPOSITORY:context.parent_repository,GITHUB_RUN_ID:'42',GITHUB_RUN_ATTEMPT:'1',GITHUB_SHA:context.parent_head_sha,
      CANARY_REPOSITORY:repository,CANARY_WORKFLOW:'rehearsal.yml',SOURCE_REVISION:context.source_revision,
      WORKSPACE_VERSION:'0.10.202',PAIRING_TEXT:'Pairs-with: consumer#7\nPairs-with: consumer#8\nConsumer-composition: consumer#9',
      GITHUB_OUTPUT:output,FIXTURE_RECORDS:records,FIXTURE_READS:reads,FIXTURE_DISPATCHES:dispatches,
      ...(scenario==='recovery'?{REHEARSAL_PROMOTION_RUN_ID:'99'}:{}),
    }});
    assert.doesNotMatch(run.stdout+run.stderr,/private-consumer|combined-private-branch|consumer-read|parent-read/);
    const calls=fs.readFileSync(dispatches,'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
    const fetched=fs.readFileSync(reads,'utf8');
    if(scenario==='measured'){
      assert.equal(run.status,0,run.stdout+run.stderr);
      assert.equal(calls.length,1);
      assert.equal(calls[0][calls[0].indexOf('--ref')+1],fixture.records.get('/pulls/9').head.ref);
      assert.deepEqual(JSON.parse(calls[0].find(arg=>arg.startsWith('observer=')).slice(9)),context);
      assert.equal(fetched.split('/pulls/8\n').length-1,4,'both source reads refresh every component');
      assert.ok(fetched.includes('/artifacts/501/zip'),'issued parent custody must be authenticated');
      assert.ok(fs.readFileSync(output,'utf8').includes(`source_declaration=${JSON.stringify(declaration())}\n`));
    }else{
      assert.notEqual(run.status,0);
      assert.equal(calls.length,0);
      assert.doesNotMatch(fs.readFileSync(output,'utf8'),/run_id=|source_declaration=/);
      assert.match(fs.readFileSync(output,'utf8'),/reason=composition_(source_unqualified|recovery_unsupported)/);
      assert.match(run.stdout+run.stderr,scenario==='recovery'?/composition_recovery_unsupported/:/consumer_source_read_refused/);
      if(scenario==='recovery')assert.equal(fetched,'');
    }
  });
}
