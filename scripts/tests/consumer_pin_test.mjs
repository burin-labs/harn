import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {load} from 'js-yaml';
import {Evaluator,Lexer,Parser,data} from '@actions/expressions';

const root=fileURLToPath(new URL('../../',import.meta.url));
const repository='fixture/private-consumer';
const mainSha='a'.repeat(40),repairSha='b'.repeat(40);
const openPull={state:'open',head:{repo:{full_name:repository},ref:'repair-branch',sha:repairSha}};

function execute(t,{body='',capturedBody=body,pull=openPull,commit=mainSha,missingRepository=false,
  parentChange={},parentReadable=true,parentToken='fixture-parent'}={}) {
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-consumer-pin-'));
  t.after(()=>fs.rmSync(scratch,{recursive:true,force:true}));
  const fixture=path.join(scratch,'fixture.json'),output=path.join(scratch,'output'),calls=path.join(scratch,'calls');
  const preload=path.join(scratch,'fetch.mjs'),script=path.join(scratch,'pin.sh');
  const parent={number:42,state:'open',body,head:{sha:'c'.repeat(40),repo:{full_name:'burin-labs/harn'}},
    base:{repo:{full_name:'burin-labs/harn'}},...parentChange};
  fs.writeFileSync(fixture,JSON.stringify({pull,commit,missingRepository,parent,parentReadable}));
  fs.writeFileSync(preload,`import fs from 'node:fs';
    const f=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
    globalThis.fetch=async(url,options={})=>{
      const p=new URL(url).pathname;
      const own=p==='/repos/burin-labs/harn/pulls/42';
      if(options.headers?.Authorization!==(own?'Bearer fixture-parent':'Bearer fixture-token'))throw new Error('wrong-authority');
      fs.appendFileSync(${JSON.stringify(calls)},p+'\\n');
      const data=own?(f.parentReadable?f.parent:null):p==='/repos/${repository}/pulls/7'?f.pull
        :p==='/repos/${repository}/commits/main'?{sha:f.commit}
        :p==='/repos/${repository}'&&!f.missingRepository?{full_name:'${repository}',default_branch:'main'}:null;
      return new Response(JSON.stringify(data??{message:'Not Found'}),{status:data?200:404});
    };`);
  const workflow=load(fs.readFileSync(path.join(root,'.github/workflows/ci.yml'),'utf8'));
  const recipe=workflow.jobs['consumer-pin'].steps.find(step=>step.id==='read').run;
  fs.writeFileSync(script,recipe);
  const run=spawnSync('bash',[script],{cwd:root,encoding:'utf8',env:{
    PATH:`${scratch}:${process.env.PATH}`,HOME:scratch,NODE_OPTIONS:`--import=${preload}`,
    GH_TOKEN:'fixture-token',PARENT_READ_TOKEN:parentToken,CONSUMER:repository,
    GITHUB_REPOSITORY:'burin-labs/harn',PULL_REQUEST_NUMBER:'42',PULL_REQUEST_HEAD_SHA:'c'.repeat(40),
    PULL_REQUEST_BODY:capturedBody,GITHUB_OUTPUT:output,
  }});
  const pinned=fs.existsSync(output)?fs.readFileSync(output,'utf8'):'';
  assert.doesNotMatch(run.stdout+run.stderr,/private-consumer|repair-branch|fixture-token/);
  return {run,pinned,reads:fs.existsSync(calls)?fs.readFileSync(calls,'utf8').trim().split('\n'):[]};
}

test('the actual CI recipe pins the paired repair, not default main',t=>{
  const {run,pinned}=execute(t,{body:'Repair contract\n\nPairs-with: consumer#7'});
  assert.equal(run.status,0,run.stderr);
  assert.equal(pinned,`sha=${repairSha}\npaired_pull=7\n`);
});

test('the actual CI recipe re-reads a pairing edited after the original event',t=>{
  const {run,pinned,reads}=execute(t,{capturedBody:'Original unpaired event',body:'Pairs-with: consumer#7'});
  assert.equal(run.status,0,run.stderr);
  assert.match(pinned,new RegExp(`^sha=${repairSha}\\n`));
  assert.match(pinned,/paired_pull=7\n/);
  assert.deepEqual(reads,['/repos/burin-labs/harn/pulls/42',`/repos/${repository}/pulls/7`]);
});

test('removing a pairing and cosmetic edits use the measured current declaration',t=>{
  const removed=execute(t,{capturedBody:'Pairs-with: consumer#7',body:''});
  assert.equal(removed.run.status,0,removed.run.stderr);
  assert.equal(removed.pinned,`sha=${mainSha}\npaired_pull=default\n`);
  const cosmetic=execute(t,{capturedBody:'Pairs-with: consumer#7',body:'Updated prose.\n\nPairs-with: consumer#7\n'});
  assert.equal(cosmetic.run.status,0,cosmetic.run.stderr);
  assert.equal(cosmetic.pinned,`sha=${repairSha}\npaired_pull=7\n`);
});

for(const [name,change] of [
  ['closed current pull',{parentChange:{state:'closed'}}],
  ['moved current head',{parentChange:{head:{sha:'d'.repeat(40),repo:{full_name:'burin-labs/harn'}}}}],
  ['foreign current head',{parentChange:{head:{sha:'c'.repeat(40),repo:{full_name:'foreign/fork'}}}}],
  ['foreign base',{parentChange:{base:{repo:{full_name:'foreign/repo'}}}}],
  ['wrong current pull number',{parentChange:{number:43}}],
  ['missing current body',{parentChange:{body:undefined}}],
  ['malformed current body',{parentChange:{body:{private:'PRIVATE-PARENT-CANARY'}}}],
  ['unreadable current pull',{parentReadable:false}],
  ['missing parent authority',{parentToken:''}],
]){
  test(`the live declaration refuses ${name} before consumer reads`,t=>{
    const result=execute(t,{body:'Pairs-with: consumer#7',...change});
    assert.notEqual(result.run.status,0);
    assert.equal(result.pinned,'');
    assert.ok(result.reads.every(endpoint=>endpoint==='/repos/burin-labs/harn/pulls/42'));
    assert.doesNotMatch(result.run.stdout+result.run.stderr,/PRIVATE-PARENT-CANARY|fixture-parent/);
  });
}

test('an explicitly null current body is a measured unpaired declaration',t=>{
  const result=execute(t,{capturedBody:'Pairs-with: consumer#7',parentChange:{body:null}});
  assert.equal(result.run.status,0,result.run.stderr);
  assert.equal(result.pinned,`sha=${mainSha}\npaired_pull=default\n`);
});

test('the same CI recipe pins a measured default head when unpaired',t=>{
  const {run,pinned}=execute(t);
  assert.equal(run.status,0,run.stderr);
  assert.equal(pinned,`sha=${mainSha}\npaired_pull=default\n`);
});

for(const [name,change] of [
  ['closed repair',{pull:{...openPull,state:'closed'}}],
  ['foreign repair',{pull:{...openPull,head:{...openPull.head,repo:{full_name:'foreign/fork'}}}}],
  ['unmeasured repair head',{pull:{...openPull,head:{...openPull.head,sha:''}}}],
  ['unreadable repair',{pull:null}],
  ['ambiguous pairing',{body:'Pairs-with: consumer#7\nPairs-with: consumer#8'}],
  ['malformed pairing',{body:'Pairs-with: consumer#unknown'}],
]) {
  test(`the actual CI recipe refuses ${name} without a fallback pin`,t=>{
    const {run,pinned}=execute(t,{body:'Pairs-with: consumer#7',...change});
    assert.notEqual(run.status,0);
    assert.equal(pinned,'');
  });
}

test('the default head must be measured through the same reader',t=>{
  const {run,pinned}=execute(t,{commit:''});
  assert.notEqual(run.status,0);
  assert.equal(pinned,'');
});

test('an unreadable default repository cannot produce a pin',t=>{
  const {run,pinned}=execute(t,{missingRepository:true});
  assert.notEqual(run.status,0);
  assert.equal(pinned,'');
});

test('candidate attempts share one captured pin and have no automatic baseline exemption jobs',()=>{
  const workflow=load(fs.readFileSync(path.join(root,'.github/workflows/ci.yml'),'utf8'));
  const budgets=JSON.parse(fs.readFileSync(path.join(root,'.github/ci-latency.json'),'utf8')).required.jobs;
  const pin=workflow.jobs['consumer-pin'];
  const read=pin.steps.find(step=>step.id==='read');
  assert.equal(pin.permissions['pull-requests'],'read');
  assert.equal(pin.outputs.paired_pull,'${{ steps.read.outputs.paired_pull }}');
  assert.equal(read.env.PARENT_READ_TOKEN,'${{ github.token }}');
  assert.equal(read.env.PULL_REQUEST_NUMBER,'${{ github.event.pull_request.number }}');
  assert.equal(read.env.PULL_REQUEST_HEAD_SHA,'${{ github.event.pull_request.head.sha }}');
  assert.equal(read.env.PULL_REQUEST_BODY,undefined);
  for(const name of ['consumer-check','consumer-check-retry']) {
    assert.equal(workflow.jobs[name].with.consumer_revision,'${{ needs.consumer-pin.outputs.sha }}');
    assert.equal(workflow.jobs[name].with.consumer_pull_request,'${{ needs.consumer-pin.outputs.paired_pull }}');
  }
  for(const name of ['consumer-base-cli','consumer-base-check','consumer-base-check-retry']) {
    assert.equal(workflow.jobs[name],undefined);
    assert.equal(budgets[name],undefined);
    assert.ok(!workflow.jobs['ci-status'].needs.includes(name));
  }
  const env=workflow.jobs['ci-status'].steps.find(step=>step.name==='Check job results')?.env
    ?? workflow.jobs['ci-status'].steps.find(step=>step.env?.CONSUMER_RESULT)?.env;
  assert.equal(env.CONSUMER_RESULT,'${{ needs.consumer-check.outputs.verdict && needs.consumer-check.result || needs.consumer-check-retry.result }}');
  assert.ok(!Object.keys(env).some(key=>key.startsWith('CONSUMER_BASE')||key==='CONSUMER_FAILURES'||key==='CONSUMER_IDENTITY'));
});

test('CI expressions bind the verdict to its actual owner before the real gate decides',()=>{
  const workflow=load(fs.readFileSync(path.join(root,'.github/workflows/ci.yml'),'utf8'));
  const env=workflow.jobs['ci-status'].steps.find(step=>step.env?.CONSUMER_RESULT).env;
  const value=input=>typeof input==='string'?new data.StringData(input):new data.Dictionary(
    ...Object.entries(input).map(([key,child])=>({key,value:value(child)})));
  const evaluate=(source,needs)=>{
    const tokens=new Lexer(source.replace(/^\$\{\{\s*|\s*\}\}$/g,'')).lex();
    const tree=new Parser(tokens.tokens,['needs'],[]).parse();
    return new Evaluator(tree,new data.Dictionary({key:'needs',value:value(needs)})).evaluate().coerceString();
  };
  const cases=[
    ['success','pass','skipped','',0],
    ['failure','','success','pass',0],
    ['cancelled','','success','pass',0],
    ['failure','pass','success','pass',1],
    ['failure','fail','success','pass',1],
    ['success','','skipped','',1],
    ['cancelled','','cancelled','',1],
  ];
  for(const [first,verdict,retry,retryVerdict,status] of cases){
    const needs={'consumer-check':{result:first,outputs:{verdict}},
      'consumer-check-retry':{result:retry,outputs:{verdict:retryVerdict}}};
    const observed={CONSUMER_RESULT:evaluate(env.CONSUMER_RESULT,needs),
      CONSUMER_VERDICT:evaluate(env.CONSUMER_VERDICT,needs)};
    const result=spawnSync('bash',['scripts/ci/consumer_verdict.sh'],{cwd:root,encoding:'utf8',
      env:{PATH:process.env.PATH,CONSUMER_EXPECTED:'true',...observed}});
    assert.equal(result.status,status,JSON.stringify({needs,observed,output:result.stdout+result.stderr}));
  }
});
