const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {snapshot,report} = require('../ci/workspace_build_evidence.cjs');

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(),'workspace-evidence-'));
  const target = path.join(root,'target');
  const folder = path.join(target,'debug/.fingerprint/probe-0123456789abcdef');
  const deps = path.join(target,'debug/deps');
  fs.mkdirSync(folder,{recursive:true});
  fs.mkdirSync(deps,{recursive:true});
  const marker = path.join(folder,'test-lib-probe');
  // Invalid JSON is intentional: opening this file is forbidden.
  fs.writeFileSync(`${marker}.json`,'UNREADABLE ENVIRONMENT CANARY');
  fs.writeFileSync(marker,'0123456789abcdef');
  const binary = path.join(deps,'probe-0123456789abcdef');
  const inventory = {'rust-build-meta':{'target-directory':target},
    'rust-suites':{probe:{status:'listed','binary-path':binary}}};
  return {root,target,folder,deps,marker,binary,inventory,
    cleanup() { fs.rmSync(root,{recursive:true,force:true}); }};
}

test('records missing binaries and changed hashes without reading fingerprint JSON', t => {
  const f = fixture();
  try {
    const original = fs.readFileSync;
    t.mock.method(fs,'readFileSync',(file,...args) => {
      assert.ok(!String(file).startsWith(f.folder) || !String(file).endsWith('.json'), 'fingerprint JSON must not be opened');
      return original(file,...args);
    });
    const before = snapshot(f.target);
    assert.equal(before.units.length,1);
    assert.equal(before.units[0].fingerprint,'0123456789abcdef');
    fs.writeFileSync(f.marker,'fedcba9876543210');
    fs.writeFileSync(f.binary,'compiled binary');
    const after = snapshot(f.target);
    const evidence = report(before,after,f.inventory);
    assert.equal(evidence.complete,true);
    assert.deepEqual(evidence.unit_changes,[{id:before.units[0].id,observation:'changed_fingerprint_hash'}]);
    assert.deepEqual(evidence.test_binaries,[{name:path.basename(f.binary),bytes:15,present_before:false}]);
    assert.ok(!JSON.stringify(evidence).includes('CANARY'));
    const warm = report(after,snapshot(f.target),f.inventory);
    assert.deepEqual(warm.unit_changes,[]);
    assert.equal(warm.test_binaries[0].present_before,true);
  } finally { f.cleanup(); }
});

test('distinguishes a new fingerprint key from a changed existing hash', () => {
  const f = fixture();
  try {
    const before = snapshot(f.target);
    const second = path.join(f.target,'debug/.fingerprint/probe-fedcba9876543210');
    fs.mkdirSync(second);
    fs.writeFileSync(path.join(second,'lib-probe.json'),'DO NOT READ');
    fs.writeFileSync(path.join(second,'lib-probe'),'1234567890abcdef');
    fs.writeFileSync(f.binary,'binary');
    const result = report(before,snapshot(f.target),f.inventory);
    assert.equal(result.unit_changes[0].observation,'new_fingerprint_key');
  } finally { f.cleanup(); }
});

test('refuses mismatched targets, missing binaries, empty inventories and non-hash markers', () => {
  const f = fixture();
  try {
    const before = snapshot(f.target);
    assert.throws(() => report(before,before,{'rust-build-meta':{'target-directory':'/wrong'}}),/target/);
    assert.throws(() => report(before,before,f.inventory),/not measured/);
    assert.throws(() => report(before,before,{...f.inventory,'rust-suites':{}}),/no test binaries/);
    fs.writeFileSync(f.marker,'ENVIRONMENT CANARY');
    assert.throws(() => snapshot(f.target),/fingerprint hash/);
  } finally { f.cleanup(); }
});

test('does not follow artifact or marker symlinks', () => {
  const f = fixture();
  try {
    fs.symlinkSync('/nonexistent-private-file',path.join(f.deps,'not-a-binary'));
    assert.deepEqual(snapshot(f.target).files,[]);
    fs.unlinkSync(f.marker);
    fs.symlinkSync(`${f.marker}.json`,f.marker);
    assert.throws(() => snapshot(f.target),/fingerprint marker/);
  } finally { f.cleanup(); }
});

test('the actual shard wrapper profiles one existing build and preserves a failed build', () => {
  const owner = path.resolve(__dirname,'../ci/run_workspace_tests.sh');
  for (const fail of [false,true]) {
    const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(),'workspace-evidence-path-')));
    try {
      const bin = path.join(root,'bin');
      const temp = path.join(root,'temp');
      fs.mkdirSync(bin);
      fs.mkdirSync(temp);
      const stub = `#!${process.execPath}
const fs=require('node:fs'),path=require('node:path');
const args=process.argv.slice(2),target=path.join(process.cwd(),'target');
if(path.basename(process.argv[1])==='cargo') {
  const names=['fixture','harn-vm','harn-cli','harn-fmt','harn-lsp','harn-package'];
  console.log(JSON.stringify({workspace_members:names,packages:names.map(name=>({name,id:name,source:null})),
    resolve:{nodes:names.map(id=>({id,features:[]}))}}));
} else {
  fs.appendFileSync(path.join(process.cwd(),'calls.jsonl'),JSON.stringify(args)+'\\n');
  if(args[1]==='list') {
    if(process.env.EVIDENCE_TEST_FAIL==='true') process.exit(23);
    fs.mkdirSync(path.join(target,'debug/deps'),{recursive:true});
    fs.mkdirSync(path.join(target,'cargo-timings'),{recursive:true});
    const binary=path.join(target,'debug/deps/fixture-0123456789abcdef');
    fs.writeFileSync(binary,'binary');
    fs.writeFileSync(path.join(target,'cargo-timings/cargo-timing.html'),'timing fixture');
    console.log(JSON.stringify({'rust-build-meta':{'target-directory':target},'test-count':1,
      'rust-suites':{fixture:{status:'listed','package-name':'fixture','binary-name':'fixture','binary-path':binary,
        testcases:{works:{ignored:false,'filter-match':{status:'matches'}}}}}}));
  }
}
`;
      for (const name of ['cargo','cargo-nextest']) fs.writeFileSync(path.join(bin,name),stub,{mode:0o755});
      const run = spawnSync('bash',[owner,'rest:1/4'],{cwd:root,encoding:'utf8',env:{...process.env,
        PATH:`${bin}:${process.env.PATH}`,GITHUB_ACTIONS:'true',GITHUB_WORKSPACE:root,
        GITHUB_OUTPUT:path.join(root,'outputs'),RUNNER_TEMP:temp,EVIDENCE_TEST_FAIL:String(fail),
        GITHUB_EVENT_NAME:'push'}});
      assert.equal(run.status,fail ? 23 : 0,run.stderr);
      const evidence = path.join(temp,'workspace-build-evidence');
      assert.ok(fs.existsSync(path.join(evidence,'before.json')));
      assert.match(fs.readFileSync(path.join(root,'outputs'),'utf8'),/workspace-build-evidence=true/);
      const calls = fs.readFileSync(path.join(root,'calls.jsonl'),'utf8').trim().split('\n').map(JSON.parse);
      assert.equal(calls.length,fail ? 1 : 2);
      assert.ok(calls[0].includes('--timings'));
      if (fail) {
        assert.equal(fs.existsSync(path.join(evidence,'report.json')),false);
      } else {
        assert.equal(calls[1].includes('--timings'),false);
        const report = JSON.parse(fs.readFileSync(path.join(evidence,'report.json'),'utf8'));
        assert.equal(report.complete,true);
        assert.equal(report.test_binaries[0].present_before,false);
        assert.equal(fs.readFileSync(path.join(evidence,'compile-timings.html'),'utf8'),'timing fixture');
      }
    } finally { fs.rmSync(root,{recursive:true,force:true}); }
  }
});
