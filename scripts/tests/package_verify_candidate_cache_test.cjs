const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const {spawnSync} = require('node:child_process');

const repository = path.resolve(__dirname,'../..');
const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(),'harn-package-candidates-')));
const runner = path.join(repository,'scripts/cargo_with_worktree_build_dir.sh');
const stamp = 1153704088; // Cargo's normalized .crate member timestamp.
function write(name, text) {
  const file = path.join(root,name);
  fs.mkdirSync(path.dirname(file),{recursive:true});
  fs.writeFileSync(file,text);
  fs.utimesSync(file,stamp,stamp);
}
const env = {...process.env,HARN_CARGO_LEASE_WORKSPACE:root,
  HARN_CARGO_LEASE_OWNER:'package-candidate-cache-fixture',
  HARN_PACKAGE_VERIFY_TARGET_DIR:path.join(root,'target'),
  HARN_PACKAGE_VERIFY_BUILD_DIR:'',CARGO_TARGET_DIR:path.join(root,'target'),
  CARGO_BUILD_BUILD_DIR:path.join(root,'target'),
  PACKAGE_TEST_REPOSITORY:repository,PACKAGE_TEST_ROOT:root,PACKAGE_TEST_RUNNER:runner,
  PACKAGE_TEST_CANDIDATE:path.join(root,'candidate')};
function command(name, program, args, expected=0) {
  const run = spawnSync(program,args,{cwd:root,env,encoding:'utf8'});
  fs.writeFileSync(path.join(root,`${name}.stdout`),run.stdout || '');
  fs.writeFileSync(path.join(root,`${name}.stderr`),run.stderr || '');
  assert.equal(run.status,expected,`${name}: ${run.error || ''}\n${run.stdout}\n${run.stderr}`);
  return run;
}
function records(run) {
  return run.stdout.split('\n').filter(line => line.startsWith('{')).map(JSON.parse);
}
function supportArtifact(run) {
  const artifact = records(run).find(r => r.reason === 'compiler-artifact' && r.target.name === 'cache_support');
  assert.ok(artifact,'the actual dependency compiler artifact must be measured');
  return artifact;
}
function verify(name, expected=0) {
  return command(name,'bash',[path.join(root,'verify.sh')],expected);
}

try {
  write('rust-toolchain.toml',fs.readFileSync(path.join(repository,'rust-toolchain.toml'),'utf8'));
  write('support/Cargo.toml','[package]\nname="cache-support"\nversion="0.1.0"\nedition="2021"\n[workspace]\n');
  write('support/src/lib.rs','pub fn value()->u32 { 1 }\n');
  write('candidate/Cargo.toml','[package]\nname="candidate"\nversion="0.1.0"\nedition="2021"\n[workspace]\n[dependencies]\ncache-support={path="../support"}\n');
  write('candidate/build.rs',[
    'fn main() {',
    'println!("cargo:rerun-if-changed=build-input.txt");',
    'let value: u32=std::fs::read_to_string("build-input.txt").unwrap().trim().parse().expect("invalid build input");',
    'let out=std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());',
    'std::fs::write(out.join("value.rs"),format!("pub const BUILD: u32 = {value};")).unwrap();',
    '}',
  ].join('\n'));
  const valid = 'include!(concat!(env!("OUT_DIR"),"/value.rs"));\npub fn value()->u32 { cache_support::value()+BUILD }\n#[test]\nfn compiled_candidate_value() { assert_eq!(value(),42); }\n';
  write('candidate/src/lib.rs',valid);
  write('candidate/build-input.txt','41\n');
  write('verify.sh',[
    '#!/usr/bin/env bash',
    'set -euo pipefail',
    'source "$PACKAGE_TEST_REPOSITORY/scripts/lib/package_verify_bootstrap.sh"',
    'external_publish_pins_table=""',
    'cargo() {',
    '  if [[ "$1" == check ]]; then',
    '    "$PACKAGE_TEST_RUNNER" "$@" --message-format=json',
    '  else',
    '    "$PACKAGE_TEST_RUNNER" "$@"',
    '  fi',
    '}',
    'package_verify_configure_target "$PACKAGE_TEST_ROOT/parent-target"',
    '[[ "$package_check_target_dir" == "$package_check_build_dir" ]]',
    'package_verify_check_resolutions "$PACKAGE_TEST_CANDIDATE/Cargo.toml" "$package_check_target_dir" "$package_check_build_dir" "$PACKAGE_TEST_ROOT" unused-filter',
  ].join('\n')+'\n');
  const first = verify('cold');
  const dependency = supportArtifact(first).filenames.find(file => file.endsWith('.rmeta'));
  assert.ok(dependency);
  const hash = () => crypto.createHash('sha256').update(fs.readFileSync(dependency)).digest('hex');
  const identity = hash();
  const warm = verify('warm');
  assert.equal(supportArtifact(warm).fresh,true);
  assert.ok(records(warm).some(r => r.reason === 'compiler-artifact' && r.target.name === 'candidate' && !r.fresh));
  const cacheTag = path.join(root,'target/CACHEDIR.TAG');
  assert.match(fs.readFileSync(cacheTag,'utf8'),/^Signature: 8a477f597d28d172789f06886806bc55\n/);
  fs.rmSync(path.join(root,'target/.rustc_info.json'),{force:true});
  assert.ok(fs.existsSync(cacheTag),'save cleanup must leave a durable nested-target marker');

  fs.renameSync(path.join(root,'candidate'),path.join(root,'relocated'));
  env.PACKAGE_TEST_CANDIDATE = path.join(root,'relocated');
  write('relocated/src/lib.rs','this deliberately does not compile\n');
  const invalid = verify('invalid-source',101);
  assert.ok(records(invalid).some(r => r.reason === 'compiler-message' && r.message.level === 'error'
    && r.message.spans.some(span => span.file_name.endsWith('src/lib.rs'))),`${invalid.stdout}\n${invalid.stderr}`);
  write('relocated/src/lib.rs',valid);
  assert.equal(supportArtifact(verify('source-recovery')).fresh,true);

  write('relocated/build-input.txt','invalid\n');
  const badBuild = verify('invalid-build-input',101);
  assert.match(badBuild.stderr,/invalid build input/);
  write('relocated/build-input.txt','41\n');
  assert.equal(supportArtifact(verify('build-recovery')).fresh,true);
  assert.equal(hash(),identity,'candidate invalidation must retain the dependency bytes');
  const built = command('tests',runner,['test','--offline','--manifest-path',path.join(env.PACKAGE_TEST_CANDIDATE,'Cargo.toml'),
    '--no-run','--message-format=json-render-diagnostics']);
  const binary = records(built).find(r => r.reason === 'compiler-artifact' && r.target.name === 'candidate' && r.profile.test)?.executable;
  assert.ok(binary,'the recovered candidate test must actually be linked');
  assert.match(command('execute',binary,['--exact','compiled_candidate_value']).stdout,/1 passed; 0 failed/);
  console.log(JSON.stringify({schema:'harn.package_candidate_cache_test.v1',
    dependency_reused:true,dependency_hash_unchanged:true,normalized_source_refused:true,
    normalized_build_input_refused:true,relocated_candidate:true,recovery_executed:true,durable_nested_target_marker:true,pending:0,bad:0}));
} finally {
  fs.rmSync(root,{recursive:true,force:true});
}
