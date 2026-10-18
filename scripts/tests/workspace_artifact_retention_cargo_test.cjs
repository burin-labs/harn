const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const {spawnSync} = require('node:child_process');
const {planRetention, applyRetention} = require('../ci/workspace_artifact_retention.cjs');

const repository = path.resolve(__dirname, '../..');
const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'harn-retention-cargo-')));
const runner = path.join(repository,'scripts/cargo_with_worktree_build_dir.sh');
function write(name, text) {
  const file = path.join(root,name);
  fs.mkdirSync(path.dirname(file),{recursive:true});
  fs.writeFileSync(file,text);
}
function command(program, args) {
  const run = spawnSync(program,args,{cwd:root,env:{...process.env,
    HARN_CARGO_LEASE_WORKSPACE:root,HARN_CARGO_LEASE_OWNER:'workspace-artifact-retention-fixture',
    CARGO_TARGET_DIR:path.join(root,'target'),CACHE_ARTIFACT_CARGO_RUNNER:runner},encoding:'utf8'});
  assert.equal(run.status,0,`${program} ${args.join(' ')}\n${run.stdout}\n${run.stderr}`);
  return run.stdout;
}
function records(name) {
  return fs.readFileSync(path.join(root,`${name}.jsonl`),'utf8').trim().split('\n').map(JSON.parse);
}
function artifacts(name) { return records(name).filter(r => r.reason === 'compiler-artifact'); }
function digest(file) { return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex'); }

try {
  write('Cargo.toml','[workspace]\nmembers=["lower","native","client"]\nresolver="2"\n');
  write('rust-toolchain.toml',fs.readFileSync(path.join(repository,'rust-toolchain.toml'),'utf8'));
  write('lower/Cargo.toml','[package]\nname="retention-lower"\nversion="0.1.0"\nedition="2021"\n[lib]\ncrate-type=["cdylib","rlib"]\n');
  write('lower/build.rs','fn main() { let out=std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()); std::fs::write(out.join("value.rs"),"pub const VALUE: u32 = 42;").unwrap(); }\n');
  write('lower/src/lib.rs','include!(concat!(env!("OUT_DIR"),"/value.rs"));\n');
  write('native/Cargo.toml','[package]\nname="harn-vm"\nversion="0.1.0"\nedition="2021"\n[dependencies]\nretention-lower={path="../lower"}\n[features]\ncurrent=[]\nsecond=[]\nretired=[]\n');
  write('native/src/lib.rs','pub fn value()->u32 { retention_lower::VALUE }\n#[test]\nfn actual_value_is_reached() { assert_eq!(value(),42); }\n');
  write('native/examples/portable_conformance_json.rs','fn main() { println!("{}",harn_vm::value()); }\n');
  write('client/Cargo.toml','[package]\nname="retention-client"\nversion="0.1.0"\nedition="2021"\n[dependencies]\nharn-vm={path="../native"}\n');
  write('client/src/main.rs','fn main() { println!("{}",harn_vm::value()); }\n');
  write('client/tests/launch.rs','#[test]\nfn compiled_binary_exists() { assert!(std::path::Path::new(env!("CARGO_BIN_EXE_retention-client")).is_file()); }\n');
  write('Makefile',[
    '.PHONY: lock retired current second metadata',
    'lock:\n\t"$(CACHE_ARTIFACT_CARGO_RUNNER)" generate-lockfile',
    ...['retired','current','second'].map(name => `${name}:\n\t"$(CACHE_ARTIFACT_CARGO_RUNNER)" test --locked ${name === 'second' ? '-p harn-vm --lib' : '--workspace'} --no-run --features harn-vm/${name === 'second' ? 'current,harn-vm/second' : name} --message-format json-render-diagnostics > ${name}.jsonl`),
    'metadata:\n\t"$(CACHE_ARTIFACT_CARGO_RUNNER)" metadata --locked --format-version 1 > metadata.json',
  ].join('\n')+'\n');
  command('make',['lock','retired','current','second','metadata']);
  const metadata = JSON.parse(fs.readFileSync(path.join(root,'metadata.json'),'utf8'));
  const workspace = fs.readFileSync(path.join(root,'current.jsonl'),'utf8');
  const security = fs.readFileSync(path.join(root,'second.jsonl'),'utf8');
  const current = new Set([...artifacts('current'),...artifacts('second')].flatMap(a => a.filenames));
  assert.ok([...current].some(file => /(?:libretention_lower\.(?:so|dylib)|retention_lower\.dll)$/.test(file)),
    'a real unhashed shared library must reach retention qualification');
  assert.ok([...current].some(file => file.includes(`${path.sep}examples${path.sep}`)),
    'a real example target must reach retention qualification');
  const stale = artifacts('retired').flatMap(a => a.filenames).filter(file => !current.has(file));
  assert.ok(stale.some(file => /harn_vm-[a-f0-9]{16}$/.test(file)), 'a real obsolete Native executable must exist');
  assert.ok(stale.some(file => file.endsWith('.rlib')), 'a real obsolete library must exist');
  const identities = new Map([...current].map(file => [file,digest(file)]));
  assert.throws(() => planRetention(metadata,workspace,''), /no Cargo records/);
  for (const file of stale) assert.ok(fs.existsSync(file),'refused inventory must not delete anything');
  const plan = planRetention(metadata,workspace,security);
  assert.equal(plan.retained_native_executables.length,2);
  assert.ok(plan.removed_bytes > 0);
  const applied = applyRetention(plan);
  for (const file of stale) assert.equal(fs.existsSync(file),false);
  for (const [file,hash] of identities) assert.equal(digest(file),hash);
  command('make',['current','second']);
  for (const name of ['current','second']) {
    const emitted = artifacts(name);
    assert.ok(emitted.length > 0);
    assert.ok(emitted.every(a => a.fresh === true), `${name} unexpectedly recompiled after pruning`);
    assert.deepEqual(records(name).at(-1),{reason:'build-finished',success:true});
    const native = emitted.find(a => a.target.name === 'harn_vm' && a.profile.test);
    assert.ok(native?.executable);
    assert.match(command(native.executable,['--exact','actual_value_is_reached']),/1 passed; 0 failed; 0 ignored/);
  }
  console.log(JSON.stringify({schema:'harn.artifact_retention_cargo_test.v1',
    current_files_unchanged:identities.size,current_native_variants:2,obsolete_files_removed:stale.length,
    removed_entries:applied.deleted_count,removed_bytes:applied.removed_bytes,fully_fresh_builds:2,pending:0,bad:0}));
} finally {
  fs.rmSync(root,{recursive:true,force:true});
}
