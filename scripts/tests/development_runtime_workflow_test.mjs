import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync,spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import test from 'node:test';

const source=fs.realpathSync(fileURLToPath(new URL('../..',import.meta.url)));

test('actual interpreter qualifier selects compatible published tools, source fallback, or an authentication refusal',()=>{
  assert.ok(process.env.HARN_BIN,'owning Make target supplies the actual interpreter');
  const binary=fs.realpathSync(process.env.HARN_BIN);
  const scratch=fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(),'harn-development-runtime-')));
  try{
    const extract=path.join(scratch,'extract.harn');
    fs.writeFileSync(extract,`import { read_yaml } from "std/fs"
fn main(harness: Harness) {
  const flow = read_yaml(harness.fs, argv[0], nil)
  let matches = 0
  for step in flow.jobs["open-development-bump"].steps {
    if step.env != nil && step.env.RUNTIME_RECEIPT != nil {
      matches = matches + 1
      harness.stdio.print(step.run)
    }
  }
  assert_eq(matches, 1)
}
`);
    const body=execFileSync(binary,['run','--no-sandbox',extract,'--',
      path.join(source,'.github/workflows/open-development-bump.yml')],{encoding:'utf8'});
    const qualifier=path.join(scratch,'qualify.sh');
    fs.writeFileSync(qualifier,body);
    const root=path.join(scratch,'workspace'),bin=path.join(scratch,'bin');
    fs.mkdirSync(path.join(root,'scripts'),{recursive:true});
    fs.mkdirSync(bin);
    fs.copyFileSync(path.join(source,'Cargo.toml'),path.join(root,'Cargo.toml'));
    fs.cpSync(path.join(source,'spec'),path.join(root,'spec'),{recursive:true});
    fs.symlinkSync(path.join(source,'conformance'),path.join(root,'conformance'),'dir');
    fs.symlinkSync(path.join(source,'crates'),path.join(root,'crates'),'dir');
    execFileSync(binary,['dump-protocol-artifacts'],{cwd:root,stdio:'pipe'});
    execFileSync('git',['init','-q','-b','main',root]);
    execFileSync('git',['-C',root,'-c','user.name=Runtime test','-c','user.email=runtime@example.com',
      '-c','commit.gpgsign=false','commit','-q','--allow-empty','-m','fixture']);
    execFileSync('git',['-C',root,'tag','v1.2.3']);
    // The existing cryptographic owner is independently exercised by its
    // product tests. Here its success/refusal is the boundary under test.
    fs.writeFileSync(path.join(root,'scripts/verify_published_harn_runtime.sh'),`#!/usr/bin/env bash
set -euo pipefail
[[ "\${AUTH_REFUSE:-0}" == 0 ]] || exit 73
jq -n --arg path "$VERIFIED_TOOL" '{path:$path}'
`,{mode:0o700});
    const tool=path.join(bin,'verified-harn');
    fs.writeFileSync(tool,`#!/usr/bin/env bash
set -euo pipefail
echo reached >> "$PROTOCOL_RECORD"
exec "$ACTUAL_INTERPRETER" "$@"
`,{mode:0o700});
    fs.writeFileSync(path.join(bin,'make'),`#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$*" >> "$SOURCE_RECORD"
[[ "$*" == setup-rust ]]
`,{mode:0o700});
    fs.writeFileSync(path.join(root,'scripts/harn_bin.sh'),`#!/usr/bin/env bash
set -euo pipefail
[[ "\${HARN_BIN-unset}" == '' && "\${HARN_BIN_NO_BUILD:-}" == 1 && "$*" == --print ]]
echo resolver >> "$SOURCE_RECORD"
printf '%s\\n' "$SOURCE_TOOL"
`,{mode:0o700});
    const protocol=path.join(scratch,'protocol'),build=path.join(scratch,'build');
    const output=path.join(scratch,'environment');
    const env={...process.env,PATH:`${bin}:${process.env.PATH}`,RUNNER_TEMP:scratch,
      RELEASE_TAG:'v1.2.3',RUNTIME_RECEIPT:'{}',VERIFIED_TOOL:tool,ACTUAL_INTERPRETER:binary,
      PROTOCOL_RECORD:protocol,SOURCE_RECORD:build,GITHUB_ENV:output,
      SOURCE_TOOL:path.join(scratch,'fresh-source-harn'),HARN_BIN:tool};
    const compatible=spawnSync('bash',[qualifier],{cwd:root,env,encoding:'utf8'});
    assert.equal(compatible.status,0,compatible.stderr);
    assert.equal(fs.readFileSync(output,'utf8'),`HARN_BIN=${tool}\n`);
    assert.equal(fs.readFileSync(protocol,'utf8'),'reached\n');
    assert.equal(fs.existsSync(build),false,'compatible published tool must not build source');

    const readme=path.join(root,'spec/protocol-artifacts/README.md');
    fs.appendFileSync(readme,'\nstale protocol control\n');
    fs.rmSync(output);fs.rmSync(protocol);
    const incompatible=spawnSync('bash',[qualifier],{cwd:root,env,encoding:'utf8'});
    assert.equal(incompatible.status,0,incompatible.stderr);
    assert.match(incompatible.stderr,/protocol artifacts are stale/);
    assert.equal(fs.readFileSync(build,'utf8'),'setup-rust\nresolver\n');
    assert.equal(fs.readFileSync(output,'utf8'),`HARN_BIN=${env.SOURCE_TOOL}\n`);
    assert.equal(fs.readFileSync(protocol,'utf8'),'reached\n');

    fs.rmSync(output);fs.rmSync(protocol);fs.rmSync(build);
    const refused=spawnSync('bash',[qualifier],{cwd:root,env:{...env,AUTH_REFUSE:'1'},encoding:'utf8'});
    assert.equal(refused.status,73);
    assert.equal(fs.existsSync(output),false);
    assert.equal(fs.existsSync(protocol),false,'unverified executable must not run');
    assert.equal(fs.existsSync(build),false,'authentication refusal must not become compatibility fallback');
  }finally{fs.rmSync(scratch,{recursive:true,force:true});}
});
