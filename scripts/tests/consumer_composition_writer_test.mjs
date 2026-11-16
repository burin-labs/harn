import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import test from 'node:test';
import {consumerCompositionIdentity} from '../lib/consumer-source.mjs';
import {compositionFixture,repository,head,declaration} from './fixtures/consumer_composition.mjs';

const source='e'.repeat(40);
const manifestDigest=`sha256:${'f'.repeat(64)}`;

for(const scenario of ['measured','missing-component','changed-child']){
  test(`actual canary binding writer ${scenario}`,t=>{
    const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'harn-composition-writer-'));
    t.after(()=>fs.rmSync(scratch,{recursive:true,force:true}));
    const fixture=compositionFixture();
    if(scenario==='missing-component')fixture.records.delete('/pulls/8');
    const records=path.join(scratch,'records.json');
    const reads=path.join(scratch,'reads.jsonl');
    const manifestReads=path.join(scratch,'manifest-reads');
    const preload=path.join(scratch,'fetch.mjs');
    const output=path.join(scratch,'output');
    const binding=path.join(scratch,'binding.json');
    fs.writeFileSync(records,JSON.stringify([...fixture.records]),{mode:0o600});
    fs.writeFileSync(reads,'');
    fs.writeFileSync(output,'');
    fs.writeFileSync(manifestReads,'');
    fs.writeFileSync(preload,`import fs from 'node:fs';
      const records=new Map(JSON.parse(fs.readFileSync(process.env.FIXTURE_RECORDS,'utf8')));
      globalThis.fetch=async(url,options)=>{
        if(options.headers.Authorization!=='Bearer consumer-read')throw new Error('wrong read authority');
        const endpoint=new URL(url);
        if(endpoint.origin!=='https://api.github.com')throw new Error('wrong endpoint');
        const key=endpoint.pathname.replace('/repos/${repository}','')+endpoint.search;
        fs.appendFileSync(process.env.FIXTURE_READS,JSON.stringify(key)+'\\n');
        const value=records.get(key);
        return new Response(JSON.stringify(value??{}),{status:value?200:404});
      };`,{mode:0o600});
    fs.writeFileSync(path.join(scratch,'gh'),`#!/usr/bin/env node
      import fs from 'node:fs';
      const args=process.argv.slice(2);
      if(args[0]!=='api'||args[1]!=='--include'||args[2]!==
        'repos/burin-labs/harn/actions/runs/42/artifacts?name=candidate-manifest-${source}')process.exit(1);
      fs.appendFileSync(process.env.FIXTURE_MANIFEST_READS,'manifest\\n');
      console.log('HTTP/2 200\\n\\n${manifestDigest}');`,{mode:0o700});
    const script=path.join(scratch,'record.sh');
    fs.writeFileSync(script,'set -euo pipefail\nsource "$1"\nCANARY_SECRET_NAME="${CANARY_REPOSITORY#*/}"\ncanary_bind "$CANARY_REPOSITORY" 77 "$ACTUAL_CHILD_HEAD"\n');
    const childHead=scenario==='changed-child'?'3'.repeat(40):head;
    const result=spawnSync('bash',[script,fileURLToPath(new URL('../ci/consumer_canary.sh',import.meta.url))],{
      encoding:'utf8',env:{PATH:`${scratch}:${process.env.PATH}`,NODE_OPTIONS:`--import=${preload}`,
        GH_TOKEN:'consumer-read',CANARY_REPOSITORY:repository,GITHUB_REPOSITORY:'burin-labs/harn',
        SOURCE_REVISION:source,CANARY_RELEASE_PRODUCER_RUN_ID:'42',CANARY_RELEASE_PRODUCER_RUN_ATTEMPT:'1',
        CANARY_VERDICT:'pass',CANARY_PAIRED_PULL:'9',CANARY_DECLARED_PULL:'9',
        CANARY_SOURCE_DECLARATION:JSON.stringify(declaration()),CANARY_BINDING_FILE:binding,
        GITHUB_OUTPUT:output,ACTUAL_CHILD_HEAD:childHead,CANARY_API_ATTEMPTS:'1',
        FIXTURE_RECORDS:records,FIXTURE_READS:reads,FIXTURE_MANIFEST_READS:manifestReads},
    });
    assert.equal(fs.readFileSync(manifestReads,'utf8'),'manifest\n');
    const observed=fs.readFileSync(reads,'utf8').trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
    assert.ok(observed.includes('/pulls/8'),'the writer must reach the required component read');
    assert.doesNotMatch(result.stdout+result.stderr,/private-consumer|combined-private-branch|consumer-read/);
    if(scenario==='measured'){
      assert.equal(result.status,0,result.stderr);
      const recorded=JSON.parse(fs.readFileSync(binding,'utf8'));
      const expectedIdentity={schema:'harn.consumer_composition_identity.v1',pull:'9',requiredPulls:['7','8'],
        manifestSha256:createHash('sha256').update('harn.consumer_composition_manifest.v1\0').update(fixture.bytes).digest('hex')};
      assert.deepEqual(recorded,{
        schema:'harn.release.consumer_rehearsal_binding.v2',source_sha:source,producer_run:'42',producer_attempt:1,
        manifest_digest:manifestDigest,consumer_run:'77',consumer_head_sha256:createHash('sha256').update(head).digest('hex'),
        consumer_pull:'9',declared_pull:'9',verdict:'pass',composition:expectedIdentity,
      });
      assert.deepEqual(consumerCompositionIdentity(recorded.composition),expectedIdentity);
      for(const number of [7,8,12,9])assert.equal(observed.filter(read=>read===`/pulls/${number}`).length,2);
      for(const privateValue of [repository,head,'a'.repeat(40),'b'.repeat(40),'c'.repeat(40),'d'.repeat(40)]){
        assert.equal(fs.readFileSync(binding,'utf8').includes(privateValue),false);
      }
      assert.equal(fs.readFileSync(output,'utf8'),`binding=${binding}\n`);
      assert.match(result.stdout,/binding=recorded/);
    }else{
      assert.notEqual(result.status,0,result.stdout);
      assert.equal(fs.existsSync(binding),false);
      assert.equal(fs.readFileSync(output,'utf8'),'');
      assert.doesNotMatch(result.stdout,/binding=recorded/);
      assert.match(result.stdout+result.stderr,scenario==='missing-component'?/consumer_source_read_refused/:/composition_source_changed/);
    }
  });
}
