import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
import {Evaluator, Lexer, Parser, data} from '@actions/expressions';
import {load} from 'js-yaml';

const root=fileURLToPath(new URL('../../',import.meta.url));
const workflow=load(fs.readFileSync(path.join(root,'.github/workflows/promote-release.yml'),'utf8'));
const consumer=workflow.jobs['consumer-rehearsal'];
const authorization=workflow.jobs.authorize.steps.find(step=>step.name==='Authenticate the fresh consumer repair');

function value(input) {
  if(input===null) return new data.Null();
  if(typeof input==='string') return new data.StringData(input);
  if(typeof input==='boolean') return new data.BooleanData(input);
  return new data.Dictionary(...Object.entries(input).map(([key,child])=>({key,value:value(child)})));
}

function runsConsumer(promote,requires,repaired,prior) {
  const context={needs:{plan:{outputs:{promote,requires_rehearsal:requires,requires_consumer_repair:repaired}}},
    inputs:{rehearsal_promotion_run_id:prior}};
  const tokens=new Lexer(consumer.if).lex();
  const parsed=new Parser(tokens.tokens,Object.keys(context),[]).parse();
  return new Evaluator(parsed,value(context)).evaluate().coerceString()==='true';
}

test('the real workflow skips only a selected prior repair rehearsal',()=>{
  assert.equal(runsConsumer('true','true','true','423'),false);
  assert.equal(runsConsumer('true','true','true',''),true);
  assert.equal(runsConsumer('true','true','false','423'),true);
  assert.equal(runsConsumer('true','false','true','423'),false);
  assert.equal(runsConsumer('false','true','true',''),false);
  const owning=workflow.jobs.authorize.steps.find(step=>step.name==='Require the owning rehearsal result');
  assert.equal(owning.env.REPAIR_REUSED,"${{ steps.consumer-repair.outputs.repair_reused || 'false' }}");
  assert.equal(owning.env.REPAIR_REQUEST,'${{ steps.consumer-repair.outputs.repair_request }}');
  assert.equal(workflow.jobs.promote.with['consumer-repair'],'${{ needs.authorize.outputs.consumer_repair }}');
});

test('the real authorization step invokes the matching authenticated reader',()=>{
  const temp=fs.mkdtempSync(path.join(os.tmpdir(),'harn-repair-workflow-'));
  try {
    const bin=path.join(temp,'bin');fs.mkdirSync(bin);
    fs.writeFileSync(path.join(bin,'node'),
      '#!/usr/bin/env bash\nprintf "%s\\n" "$@" > "$CALLS"\nprintf "%s" "${OBSERVER_CONTEXT:-}" > "$CONTEXT"\nexit "${FIXTURE_EXIT:-0}"\n',
      {mode:0o755});
    const step=path.join(temp,'authorization.sh');fs.writeFileSync(step,authorization.run);
    const invoke=(prior,exit='0')=>{
      const calls=path.join(temp,'calls');const context=path.join(temp,'context');
      const run=spawnSync('bash',['-e','-o','pipefail',step],{encoding:'utf8',env:{
        ...process.env,PATH:`${bin}:${process.env.PATH}`,CALLS:calls,CONTEXT:context,FIXTURE_EXIT:exit,
        PARENT_RUN_ID:prior,ISSUED_CONTEXT:'{"fixture":true}',PRODUCER_RUN:'123',
        PRODUCER_ATTEMPT:'1',CONSUMER_PULL:'7',
      }});
      return {run,calls:fs.readFileSync(calls,'utf8').trim().split('\n'),context:fs.readFileSync(context,'utf8')};
    };
    const prior=invoke('423');assert.equal(prior.run.status,0,prior.run.stderr);
    assert.equal(prior.calls.at(-1),'--prior-repair');
    const fresh=invoke('');assert.equal(fresh.run.status,0,fresh.run.stderr);
    assert.equal(fresh.calls.at(-1),'--repair');
    assert.deepEqual(JSON.parse(fresh.context),{schema:'harn.release.consumer_repair.v1',context:{fixture:true},
      producer_run:'123',producer_attempt:1,consumer_pull:'7'});
    assert.notEqual(invoke('423','7').run.status,0,'reader refusal must fail authorization');
  } finally {
    fs.rmSync(temp,{recursive:true,force:true});
  }
});
