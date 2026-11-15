import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import test from 'node:test';

const root=fileURLToPath(new URL('../../',import.meta.url));

test('same recorded job and step cannot excuse 28 executed failures with a base import failure',t=>{
  const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-consumer-failure-'));
  t.after(()=>fs.rmSync(scratch,{recursive:true,force:true}));
  const jobs={total_count:1,jobs:[{name:'Pipeline shard 1',conclusion:'failure',
    steps:[{name:'Run pipeline tests',conclusion:'failure'}]}]};
  fs.writeFileSync(path.join(scratch,'jobs.json'),JSON.stringify(jobs));
  const candidate={cases:Array.from({length:28},(_,i)=>({name:`case_${i}`,outcome:'failed',phase:'execute'}))};
  const baseline={cases:[{name:'module_import',outcome:'failed',phase:'compile'}]};
  assert.equal(candidate.cases.length,28);
  assert.equal(baseline.cases.length,1);
  assert.notDeepEqual(candidate,baseline);
  fs.writeFileSync(path.join(scratch,'candidate.json'),JSON.stringify(candidate));
  fs.writeFileSync(path.join(scratch,'baseline.json'),JSON.stringify(baseline));
  const gh=path.join(scratch,'gh');
  fs.writeFileSync(gh,`#!/usr/bin/env node
import fs from 'node:fs';
import {spawnSync} from 'node:child_process';
const args=process.argv.slice(2),endpoint=args.find(value=>value.startsWith('repos/'));
const body=endpoint.includes('/jobs?')?JSON.parse(fs.readFileSync(process.env.FIXTURE_JOBS,'utf8')):{head_sha:'a'.repeat(40)};
const query=args[args.indexOf('--jq')+1];
const filtered=spawnSync('jq',['-r',query],{input:JSON.stringify(body),encoding:'utf8'});
if(filtered.status!==0)process.exit(filtered.status??1);
process.stdout.write('HTTP/2.0 200 OK\\r\\n\\r\\n'+filtered.stdout);
`,{mode:0o755});
  const records=[];
  for(const run of ['11','22']){
    const output=path.join(scratch,`output-${run}`);
    const recorded=spawnSync('bash',['scripts/ci/consumer_canary.sh','--record'],{cwd:root,encoding:'utf8',env:{
      PATH:`${scratch}:${process.env.PATH}`,FIXTURE_JOBS:path.join(scratch,'jobs.json'),
      CANARY_REPOSITORY:'fixture/private-consumer',CANARY_RECORD_RUN:run,GITHUB_OUTPUT:output,
    }});
    assert.equal(recorded.status,0,recorded.stderr+recorded.stdout);
    records.push(Object.fromEntries(fs.readFileSync(output,'utf8').trim().split('\n').map(line=>{
      const split=line.indexOf('=');return [line.slice(0,split),line.slice(split+1)];
    })));
  }
  assert.match(records[0].consumer_failures,/^[0-9a-f]{64}$/);
  assert.match(records[0].consumer_jobs,/^[0-9a-f]{64}$/);
  assert.match(records[0].consumer_head,/^[0-9a-f]{40}$/);
  assert.equal(records[0].consumer_failures,records[1].consumer_failures);
  assert.equal(records[0].consumer_jobs,records[1].consumer_jobs);
  assert.equal(records[0].consumer_head,records[1].consumer_head);
  const result=spawnSync('bash',['scripts/ci/consumer_verdict.sh'],{cwd:root,encoding:'utf8',env:{
    PATH:process.env.PATH,CONSUMER_EXPECTED:'true',CONSUMER_RESULT:'failure',
    CONSUMER_VERDICT:'fail',CONSUMER_BASE_VERDICT:'fail',CONSUMER_BASE_CLI_RESULT:'success',
    CONSUMER_IDENTITY:`${records[0].consumer_head}/${records[0].consumer_jobs}`,
    CONSUMER_BASE_IDENTITY:`${records[1].consumer_head}/${records[1].consumer_jobs}`,
    CONSUMER_FAILURES:records[0].consumer_failures,CONSUMER_BASE_FAILURES:records[1].consumer_failures,
  }});
  assert.equal(result.status,1,result.stdout+result.stderr);
  assert.match(result.stdout,/::error::.*are incomparable/);
});
