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

function execute(t,{body='',pull=openPull,commit=mainSha,missingRepository=false}={}) {
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-consumer-pin-'));
  t.after(()=>fs.rmSync(scratch,{recursive:true,force:true}));
  const fixture=path.join(scratch,'fixture.json'),output=path.join(scratch,'output');
  const preload=path.join(scratch,'fetch.mjs'),gh=path.join(scratch,'gh'),script=path.join(scratch,'pin.sh');
  fs.writeFileSync(fixture,JSON.stringify({pull,commit,missingRepository}));
  fs.writeFileSync(preload,`import fs from 'node:fs';
    const f=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
    globalThis.fetch=async(url,options={})=>{
      if(options.headers?.Authorization!=='Bearer fixture-token')throw new Error('wrong-authority');
      const p=new URL(url).pathname;
      const data=p==='/repos/${repository}/pulls/7'?f.pull
        :p==='/repos/${repository}/commits/main'?{sha:f.commit}
        :p==='/repos/${repository}'&&!f.missingRepository?{full_name:'${repository}',default_branch:'main'}:null;
      return new Response(JSON.stringify(data??{message:'Not Found'}),{status:data?200:404});
    };`);
  // The pre-fix workflow calls gh directly; execute that real recipe too.
  fs.writeFileSync(gh,`#!/usr/bin/env node
    import fs from 'node:fs';
    const f=JSON.parse(fs.readFileSync(${JSON.stringify(fixture)},'utf8'));
    const endpoint=process.argv[3];
    console.log(endpoint.endsWith('/commits/main')?f.commit:'main');
  `,{mode:0o755});
  const workflow=load(fs.readFileSync(path.join(root,'.github/workflows/ci.yml'),'utf8'));
  const recipe=workflow.jobs['consumer-pin'].steps.find(step=>step.id==='read').run;
  fs.writeFileSync(script,recipe);
  const run=spawnSync('bash',[script],{cwd:root,encoding:'utf8',env:{
    PATH:`${scratch}:${process.env.PATH}`,HOME:scratch,NODE_OPTIONS:`--import=${preload}`,
    GH_TOKEN:'fixture-token',CONSUMER:repository,PULL_REQUEST_BODY:body,GITHUB_OUTPUT:output,
  }});
  const pinned=fs.existsSync(output)?fs.readFileSync(output,'utf8'):'';
  assert.doesNotMatch(run.stdout+run.stderr,/private-consumer|repair-branch|fixture-token/);
  return {run,pinned};
}

test('the actual CI recipe pins the paired repair, not default main',t=>{
  const {run,pinned}=execute(t,{body:'Repair contract\n\nPairs-with: consumer#7'});
  assert.equal(run.status,0,run.stderr);
  assert.equal(pinned,`sha=${repairSha}\n`);
});

test('the same CI recipe pins a measured default head when unpaired',t=>{
  const {run,pinned}=execute(t);
  assert.equal(run.status,0,run.stderr);
  assert.equal(pinned,`sha=${mainSha}\n`);
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
  for(const name of ['consumer-check','consumer-check-retry']) {
    assert.equal(workflow.jobs[name].with.consumer_revision,'${{ needs.consumer-pin.outputs.sha }}');
  }
  for(const name of ['consumer-base-cli','consumer-base-check','consumer-base-check-retry']) {
    assert.equal(workflow.jobs[name],undefined);
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
