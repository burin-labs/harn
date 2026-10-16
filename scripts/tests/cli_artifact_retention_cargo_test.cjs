const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const {spawnSync} = require('node:child_process');
const {planCliRetention} = require('../ci/workspace_artifact_retention.cjs');

const repository = path.resolve(__dirname,'../..');
const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(),'harn-cli-retention-')));
const runner = path.join(repository,'scripts/cargo_with_worktree_build_dir.sh');
const env = {...process.env,HARN_CARGO_LEASE_WORKSPACE:root,HARN_CARGO_LEASE_OWNER:'cli-retention-fixture',
  CARGO_TARGET_DIR:path.join(root,'target'),CARGO_BUILD_BUILD_DIR:path.join(root,'target'),CARGO_INCREMENTAL:'0',
  CLI_TEST_ROOT:root,CLI_TEST_RUNNER:runner,PATH:`${path.join(root,'bin')}${path.delimiter}${process.env.PATH}`};
function write(name, text) {
  const file = path.join(root,name);
  fs.mkdirSync(path.dirname(file),{recursive:true});
  fs.writeFileSync(file,text);
}
function command(name, program, args) {
  const run = spawnSync(program,args,{cwd:root,env:program === runner ? {...env,PATH:process.env.PATH} : env,encoding:'utf8'});
  assert.equal(run.status,0,`${name}: ${run.error || ''}\n${run.stdout}\n${run.stderr}`);
  return run.stdout;
}
function buildRecords() {
  return fs.readFileSync(path.join(root,'last-build.jsonl'),'utf8').trim().split('\n').map(JSON.parse);
}
function metadata() { return JSON.parse(command('metadata',runner,['metadata','--locked','--format-version','1'])); }
function encode(records) { return records.map(r => JSON.stringify(r)).join('\n')+'\n'; }
function digest(file) { return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex'); }
function nativeManifest(feature) {
  return `[package]\nname="harn-vm"\nversion="0.1.0"\nedition="2021"\n[features]\ndefault=["${feature}"]\ncurrent=[]\nretired=[]\n`;
}
const warm = name => command(name,'bash',[path.join(repository,'scripts/ci/warm_harn_cli_cache.sh')]);

try {
  write('Cargo.toml','[workspace]\nmembers=["native","cli","generator"]\nresolver="2"\n[profile.ci-cli]\ninherits="dev"\n');
  write('rust-toolchain.toml',fs.readFileSync(path.join(repository,'rust-toolchain.toml'),'utf8'));
  write('native/Cargo.toml',nativeManifest('retired'));
  write('native/src/lib.rs','pub fn value()->u32 { if cfg!(feature="retired") { 13 } else { 42 } }\n');
  write('native/build.rs','fn main() { println!("cargo:rerun-if-changed=build.rs"); }\n');
  write('cli/Cargo.toml','[package]\nname="harn-cli"\nversion="0.1.0"\nedition="2021"\n[dependencies]\nharn-vm={path="../native"}\n[[bin]]\nname="harn"\npath="src/main.rs"\n');
  write('cli/src/main.rs','fn main() { println!("{}",harn_vm::value()); }\n');
  write('generator/Cargo.toml','[package]\nname="harn-cli-aot-gen"\nversion="0.1.0"\nedition="2021"\n[dependencies]\nharn-vm={path="../native"}\n');
  write('generator/src/main.rs','fn main() { println!("generator{}",harn_vm::value()); }\n');
  write('bin/cargo',[
    '#!/usr/bin/env bash','set -euo pipefail','export PATH="${PATH#"$CLI_TEST_ROOT/bin:"}"','if [[ "$1" == build ]]; then',
    '  "$CLI_TEST_RUNNER" "$@" > "$CLI_TEST_ROOT/last-build.jsonl"',
    '  cat "$CLI_TEST_ROOT/last-build.jsonl"','else','  exec "$CLI_TEST_RUNNER" "$@"','fi',
  ].join('\n')+'\n');
  fs.chmodSync(path.join(root,'bin/cargo'),0o755);
  command('lockfile',runner,['generate-lockfile']);
  warm('retired graph');
  assert.equal(command('retired CLI',path.join(root,'target/ci-cli/harn'),[]),'13\n');
  const previous = buildRecords();
  const deps = path.join(root,'target/ci-cli/deps');
  const oldBinaries = fs.readdirSync(deps).filter(name => /^(?:harn|harn_cli_aot_gen)-[a-f0-9]{16}$/.test(name));
  assert.equal(oldBinaries.length,2,'both actual hashed CLI executables must be measured');
  write('native/Cargo.toml',nativeManifest('current'));
  const output = warm('current graph');
  const current = buildRecords();
  const currentFiles = new Set(current.flatMap(r => r.filenames || []));
  const obsolete = previous.flatMap(r => r.filenames || []).filter(file => !currentFiles.has(file));
  assert.ok(obsolete.some(file => file.endsWith('.rlib')));
  for (const file of obsolete) assert.equal(fs.existsSync(file),false);
  for (const file of oldBinaries) assert.equal(fs.existsSync(path.join(deps,file)),false);
  const identities = new Map([...currentFiles].map(file => [file,digest(file)]));
  const meta = metadata();
  const plan = planCliRetention(meta,encode(current));
  assert.equal(plan.profile,'ci-cli');
  assert.equal(plan.retained_cli_executables.length,2);
  assert.equal(plan.removed.length,0);
  assert.equal(plan.pending,0);
  assert.equal(plan.bad,0);
  assert.throws(() => planCliRetention(meta,''),/no Cargo records/);
  assert.throws(() => planCliRetention(meta,encode(current.filter(r => r.target?.name !== 'harn-cli-aot-gen'))),/missing CLI targets/);
  assert.throws(() => planCliRetention(meta,encode(current.filter(r => !String(r.package_id).includes('#harn-vm@')))),/Native libraries=0/);
  const named = path.join(root,'target/ci-cli/harn');
  const stat = fs.statSync(named,{bigint:true});
  const hashed = fs.readdirSync(deps).map(name => path.join(deps,name)).find(file => {
    const candidate = fs.lstatSync(file,{bigint:true});
    return candidate.isFile() && candidate.dev === stat.dev && candidate.ino === stat.ino;
  });
  assert.ok(hashed);
  fs.unlinkSync(named);
  fs.copyFileSync(hashed,named);
  assert.throws(() => planCliRetention(meta,encode(current)),/no matching hashed Cargo executable/);
  fs.unlinkSync(named);
  fs.linkSync(hashed,named);
  // The pinned cache cleanup removes named outputs and incremental state.
  // A consumer must recover those names from the retained hashed outputs.
  const profile = path.join(root,'target/ci-cli');
  for (const entry of fs.readdirSync(profile)) {
    if (!['build','.fingerprint','deps'].includes(entry)) fs.rmSync(path.join(profile,entry),{recursive:true});
  }
  fs.rmSync(path.join(root,'target/.rustc_info.json'),{force:true});
  warm('unchanged graph');
  const fresh = buildRecords().filter(r => r.reason === 'compiler-artifact');
  assert.ok(fresh.length > 0 && fresh.every(r => r.fresh));
  for (const [file,hash] of identities) assert.equal(digest(file),hash);
  assert.equal(command('CLI',named,[]),'42\n');
  assert.equal(command('AOT generator',path.join(root,'target/ci-cli/harn-cli-aot-gen'),[]),'generator42\n');
  const receipt = JSON.parse(output.trim().split('\n').at(-1));
  assert.ok(receipt.deleted_count > 0 && receipt.removed_bytes > 0);
  console.log(JSON.stringify({schema:'harn.cli_artifact_retention_test.v1',current_files_unchanged:identities.size,
    retired_cli_executables_removed:oldBinaries.length,obsolete_outputs_removed:obsolete.length,
    removed_bytes:receipt.removed_bytes,fully_fresh_builds:1,cache_cleanup_shape_reused:true,executed_cli_artifacts:2,pending:0,bad:0}));
} finally { fs.rmSync(root,{recursive:true,force:true}); }
