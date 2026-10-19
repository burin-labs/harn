import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {consumerQueue, writeConsumerFailure} from '../lib/consumer-observer-failure.mjs';

function failureReceiptOutput(text, directory) {
  const lines = text.trimEnd().split('\n');
  assert.equal(lines.length, 1, 'only the private receipt path may be output');
  assert.match(lines[0], /^failure_receipt=.+$/);
  const file = lines[0].slice('failure_receipt='.length);
  assert.equal(path.dirname(path.dirname(file)), directory);
  assert.equal(path.basename(file), 'failure.json');
  return file;
}

test('queue receipts distinguish measured empty, measured backlog and unknown without names or logs', async () => {
  const read = record => consumerQueue('fixture', 'acme/widget-host', 'rehearsal.yml', async url => {
    assert.equal(new URL(url).searchParams.get('status'), 'queued');
    return {ok:true, json:async()=>record};
  });
  assert.deepEqual(await read({total_count:0, workflow_runs:[]}), {depth:0, pending_runs:[]});
  const run = {id:73, run_attempt:2, status:'queued'};
  assert.deepEqual(await read({total_count:1, workflow_runs:[run]}),
    {depth:1, pending_runs:[{run_id:'73', run_attempt:2}]});
  for (const record of [{}, {total_count:1,workflow_runs:[]},
    {total_count:1,workflow_runs:[{...run,run_attempt:undefined}]},
    {total_count:1,workflow_runs:[{...run,status:'unknown'}]}]) {
    assert.deepEqual(await read(record), {depth:null,pending_runs:null});
  }
  // Identifiers can occur in an opaque local path without becoming output fields.
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-failure-test-73-widget-host-'));
  try {
    const output = path.join(directory, 'output');
    const receipt = await writeConsumerFailure({GH_TOKEN:'fixture', CANARY_REPOSITORY:'acme/widget-host',
      CANARY_WORKFLOW:'rehearsal.yml', CANARY_RUN_ID:'72', RUNNER_TEMP:directory, GITHUB_OUTPUT:output},
    'no_verdict_before_deadline', async()=>({ok:true,json:async()=>({total_count:1,workflow_runs:[run]})}));
    const outputText = fs.readFileSync(output,'utf8');
    const file = failureReceiptOutput(outputText, directory);
    assert.deepEqual(JSON.parse(fs.readFileSync(file,'utf8')), receipt);
    assert.equal(receipt.queue.depth,1);
    assert.deepEqual(receipt.queue.pending_runs,[{run_id:'73',run_attempt:2}]);
    assert.equal(fs.statSync(file).mode & 0o777, 0o600);
    assert.equal(fs.statSync(path.dirname(file)).mode & 0o777, 0o700);
    assert.throws(() => failureReceiptOutput(`${outputText}run_id=73\n`, directory),
      /only the private receipt path may be output/);
  } finally {
    fs.rmSync(directory,{recursive:true});
  }
});

test('the closed candidate entrypoint cancels exact live parents and preserves unrelated or needed runs', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-retire-test-'));
  try {
    const config = path.join(directory,'config.json');
    const calls = path.join(directory,'calls.json');
    const event = path.join(directory,'event.json');
    const preload = path.join(directory,'fetch.mjs');
    const repository = 'burin-labs/harn';
    const sha = 'a'.repeat(40);
    const pull = {number:7,state:'closed',base:{repo:{full_name:repository}},
      head:{sha,ref:'candidate',repo:{full_name:repository}}};
    const parent = {id:42,run_attempt:1,head_sha:sha,head_branch:'candidate',
      head_repository:{full_name:repository},event:'pull_request',
      path:'.github/workflows/build-release-binaries.yml',status:'in_progress'};
    fs.writeFileSync(event,JSON.stringify({repository:{full_name:repository},action:'closed',number:7}));
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const fixture=JSON.parse(fs.readFileSync(${JSON.stringify(config)},'utf8'));
      const calls=[];
      let pullReads=0;
      globalThis.fetch=async(url,options={})=>{
        const endpoint=new URL(url);
        calls.push({path:endpoint.pathname,method:options.method??'GET'});
        fs.writeFileSync(${JSON.stringify(calls)},JSON.stringify(calls));
        if(endpoint.pathname.endsWith('/cancel'))return {status:202};
        if(endpoint.pathname.endsWith('/pulls/7'))return {ok:true,json:async()=>++pullReads>1&&fixture.finalPull?fixture.finalPull:fixture.pull};
        if(endpoint.pathname.endsWith('/runs/42'))return {ok:true,json:async()=>fixture.parent};
        if(endpoint.pathname.endsWith('/build-release-binaries.yml/runs'))return {ok:true,json:async()=>fixture.census??{total_count:1,workflow_runs:[fixture.parent]}};
        if(endpoint.pathname.endsWith('/ci.yml/runs'))return {ok:true,json:async()=>({total_count:0,workflow_runs:[]})};
        throw new Error('unknown fixture endpoint');
      };`);
    const entry = fileURLToPath(new URL('../ci/consumer-observer-retire.mjs',import.meta.url));
    const execute = changes => {
      fs.writeFileSync(config,JSON.stringify({pull,parent,...changes}));
      fs.writeFileSync(calls,'[]');
      const result=spawnSync(process.execPath,[entry],{encoding:'utf8',env:{PATH:process.env.PATH,
        NODE_OPTIONS:`--import=${preload}`,GH_TOKEN:'fixture',GITHUB_EVENT_NAME:'pull_request_target',
        GITHUB_REPOSITORY:repository,GITHUB_EVENT_PATH:event}});
      return {result,posts:JSON.parse(fs.readFileSync(calls,'utf8')).filter(call=>call.method==='POST')};
    };
    const retired=execute({});
    assert.equal(retired.result.status,0,retired.result.stderr);
    assert.deepEqual(retired.posts,[{path:'/repos/burin-labs/harn/actions/runs/42/cancel',method:'POST'}]);
    assert.match(retired.result.stdout,/discovered=1 cancelled=1 terminal=0 pending=0 refused=0/);
    for(const reading of [{...parent,event:'workflow_dispatch'}, {...parent,head_sha:'b'.repeat(40)},
      {...parent,head_branch:'needed'}, {...parent,head_repository:{full_name:'acme/fork'}},
      {...parent,status:'completed',conclusion:'success'}]) {
      const preserved=execute({parent:reading});
      assert.equal(preserved.result.status,0,preserved.result.stderr);
      assert.equal(preserved.posts.length,0);
    }
    for(const changes of [{pull:{...pull,state:'open'}},{pull:{}},{finalPull:{...pull,state:'open'}},
      {census:{total_count:1,workflow_runs:[]}}]) {
      const unknown=execute(changes);
      assert.equal(unknown.result.status,1);
      assert.equal(unknown.posts.length,0);
    }
  } finally {
    fs.rmSync(directory,{recursive:true});
  }
});
