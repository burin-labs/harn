const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {planRetention, applyRetention} = require('../ci/workspace_artifact_retention.cjs');

function fixture() {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-current-artifacts-'));
  const root = path.join(workspace, 'target');
  for (const name of ['debug/deps', 'debug/build', 'debug/.fingerprint']) fs.mkdirSync(path.join(root, name), {recursive:true});
  const unit = {name:'harn_vm',kind:['lib'],test:true};
  const integration = {name:'harn_vm',kind:['test'],test:true};
  const hostUnit = {name:'harn_hostlib',kind:['lib'],test:true};
  const custom = {name:'build-script-build',kind:['custom-build'],test:false};
  const vm = {id:'vm',name:'harn-vm',manifest_path:path.join(workspace,'vm/Cargo.toml'),targets:[unit,integration,custom]};
  const host = {id:'host',name:'harn-hostlib',manifest_path:path.join(workspace,'host/Cargo.toml'),targets:[hostUnit]};
  const metadata = {workspace_root:workspace,target_directory:root,workspace_members:['vm','host'],packages:[vm,host],resolve:{nodes:[]}};
  function file(relative) {
    const result = path.join(root, relative);
    fs.mkdirSync(path.dirname(result), {recursive:true});
    fs.writeFileSync(result, `artifact ${relative}`);
    return result;
  }
  function artifact(pkg, target, digit, isTest) {
    const hash = digit.repeat(16);
    const stem = `${isTest ? '' : 'lib'}${target.name}-${hash}`;
    const filenames = [file(`debug/deps/${stem}${isTest ? '' : '.rlib'}`)];
    if (isTest) fs.chmodSync(filenames[0],0o755);
    if (!isTest) filenames.push(file(`debug/deps/${stem}.rmeta`));
    file(`debug/deps/${target.name}-${hash}.d`);
    return {reason:'compiler-artifact',package_id:pkg.id,manifest_path:pkg.manifest_path,
      target,profile:{test:isTest},fresh:true,filenames,executable:isTest ? filenames[0] : null};
  }
  const first = [artifact(vm,unit,'1',true),artifact(vm,unit,'2',false),
    artifact(vm,integration,'3',true),artifact(host,hostUnit,'4',true)];
  const second = [artifact(vm,unit,'5',true),artifact(vm,unit,'6',false)];
  const buildBinary = file(`debug/build/harn-vm-${'7'.repeat(16)}/build-script-build`);
  first.push({reason:'compiler-artifact',package_id:vm.id,manifest_path:vm.manifest_path,
    target:custom,profile:{test:false},fresh:true,filenames:[buildBinary],executable:null});
  const out = path.join(root, `debug/build/harn-vm-${'8'.repeat(16)}/out`);
  fs.mkdirSync(out, {recursive:true});
  first.push({reason:'build-script-executed',package_id:vm.id,out_dir:out});
  const stale = [file(`debug/deps/harn_vm-${'a'.repeat(16)}`),
    file(`debug/deps/libharn_vm-${'b'.repeat(16)}.rlib`),
    path.dirname(file(`debug/build/harn-vm-${'c'.repeat(16)}/obsolete-output`))];
  const foreign = file(`debug/deps/libserde-${'d'.repeat(16)}.rlib`);
  const fingerprint = file(`debug/.fingerprint/harn-vm-${'a'.repeat(16)}/test-lib-harn_vm`);
  const kept = [...first,...second].flatMap(a => a.filenames || []);
  const text = records => records.map(r => JSON.stringify(r)).join('\n')+'\n';
  const ending = {reason:'build-finished',success:true};
  return {workspace,root,metadata,first,second,stale,foreign,fingerprint,kept,text,
    plan(a = first, b = second) { return planRetention(metadata,text([...a,ending]),text([...b,ending])); },
    cleanup() { fs.rmSync(workspace,{recursive:true,force:true}); }};
}

test('both current feature graphs survive obsolete-output pruning and a measured second plan is empty', () => {
  const f = fixture();
  try {
    const identities = f.kept.map(file => [file,fs.readFileSync(file,'utf8')]);
    const plan = f.plan();
    assert.equal(plan.schema,'harn.workspace_artifact_retention.v1');
    assert.equal(plan.retained_native_executables.length, 3);
    assert.equal(plan.pending, 0);
    assert.equal(plan.bad, 0);
    assert.equal(plan.removed.length, f.stale.length);
    const applied = applyRetention(plan);
    assert.equal(applied.deleted_count, f.stale.length);
    for (const file of f.stale) assert.equal(fs.existsSync(file),false);
    for (const [file,bytes] of identities) assert.equal(fs.readFileSync(file,'utf8'),bytes);
    assert.equal(fs.existsSync(f.foreign),true);
    assert.equal(fs.existsSync(f.fingerprint),true);
    const again = f.plan();
    assert.equal(again.removed.length,0);
    assert.equal(again.retained_native_executables.length,3);
    assert.ok(again.current_files > 0);
  } finally { f.cleanup(); }
});

for (const [label, alter] of [
  ['empty', () => ''],
  ['malformed', () => '{invalid}\n'],
  ['missing terminal', text => text.split('\n').filter(line => !line.includes('build-finished')).join('\n')],
  ['failed build', text => text.replace('"success":true','"success":false')],
]) {
  test(`${label} Cargo inventory refuses before any deletion`, () => {
    const f = fixture();
    try {
      const text = f.text([...f.first,{reason:'build-finished',success:true}]);
      assert.throws(() => planRetention(f.metadata,alter(text),f.text([...f.second,{reason:'build-finished',success:true}])));
      for (const file of f.stale) assert.equal(fs.existsSync(file),true);
    } finally { f.cleanup(); }
  });
}

test('an integration binary cannot conceal a missing same-named library test target', () => {
  const f = fixture();
  try {
    const partial = f.first.filter(a => !(a.reason === 'compiler-artifact' && a.target.kind[0] === 'lib' && a.profile.test && a.package_id === 'vm'));
    assert.throws(() => f.plan(partial), /missing 1 test targets/);
    for (const file of f.stale) assert.equal(fs.existsSync(file),true);
  } finally { f.cleanup(); }
});

test('a security phase without Native execution artifacts refuses', () => {
  const f = fixture();
  try { assert.throws(() => f.plan(f.first,[]), /did not measure current Native executables/); }
  finally { f.cleanup(); }
});

test('a same-named integration executable cannot stand in for the Native unit graph', () => {
  const f = fixture();
  try {
    assert.throws(() => f.plan(f.first,[f.first[2]]), /did not measure current Native executables/);
    for (const file of f.stale) assert.equal(fs.existsSync(file),true);
  } finally { f.cleanup(); }
});

test('a current dependency with a workspace target name remains protected', () => {
  const f = fixture();
  try {
    const file = path.join(f.root,`debug/deps/libharn_vm-${'e'.repeat(16)}.rlib`);
    fs.writeFileSync(file,'current external dependency');
    const dependency = {...f.first[1],package_id:'external',filenames:[file]};
    const plan = f.plan([...f.first,dependency]);
    assert.equal(plan.protected_dependency_files,1);
    applyRetention(plan);
    assert.equal(fs.readFileSync(file,'utf8'),'current external dependency');
  } finally { f.cleanup(); }
});

test('an unhashed shared library requires the matching Cargo crate type and exact target name', () => {
  const f = fixture();
  try {
    const target = {...f.metadata.packages[1].targets[0],crate_types:['cdylib','rlib']};
    f.metadata.packages[1].targets[0] = target;
    const file = path.join(f.root,'debug/deps/libharn_hostlib.so');
    fs.writeFileSync(file,'current shared library');
    const record = {...f.first[3],target,profile:{test:false},filenames:[file],executable:null};
    applyRetention(f.plan([...f.first,record]));
    assert.equal(fs.readFileSync(file,'utf8'),'current shared library');
    const rlib = path.join(f.root,'debug/deps/libharn_hostlib.rlib');
    fs.writeFileSync(rlib,'current companion library');
    assert.doesNotThrow(() => f.plan([...f.first,{...record,filenames:[rlib]}]));
    target.crate_types = ['cdylib'];
    assert.throws(() => f.plan([...f.first,{...record,filenames:[rlib]}]), /unsupported dependency artifact/);
    target.crate_types = ['rlib'];
    assert.throws(() => f.plan([...f.first,record]), /unsupported dependency artifact/);
    target.crate_types = ['cdylib','rlib'];
    record.filenames = [f.foreign];
    assert.throws(() => f.plan([...f.first,record]), /unsupported dependency artifact/);
  } finally { f.cleanup(); }
});

test('a foreign output path and a missing current artifact refuse before pruning', () => {
  const f = fixture();
  try {
    const foreign = structuredClone(f.first);
    foreign[0].filenames[0] = path.join(f.workspace,'source.rs');
    foreign[3].filenames[0] = path.join(f.workspace,'second.rs');
    assert.throws(() => f.plan(foreign), error => error.failures?.length === 2
      && error.failures.every(message => message.includes('escapes target'))
      && error.failures.some(message => message.includes('source.rs'))
      && error.failures.some(message => message.includes('second.rs')));
    fs.rmSync(f.first[0].filenames[0]);
    assert.throws(() => f.plan(), /ENOENT/);
    for (const file of f.stale) assert.equal(fs.existsSync(file),true);
  } finally { f.cleanup(); }
});

test('a changed deletion candidate refuses the entire mutation before its first removal', () => {
  const f = fixture();
  try {
    const plan = f.plan();
    fs.appendFileSync(f.stale[1],'changed');
    assert.throws(() => applyRetention(plan), /changed after census/);
    assert.equal(plan.deleted_count,0);
    for (const file of f.stale) assert.equal(fs.existsSync(file),true);
  } finally { f.cleanup(); }
});

test('a Native artifact without executable permission refuses before pruning', () => {
  const f = fixture();
  try {
    fs.chmodSync(f.first[0].filenames[0],0o644);
    assert.throws(() => f.plan(), /Native artifact is not executable/);
    for (const file of f.stale) assert.equal(fs.existsSync(file),true);
  } finally { f.cleanup(); }
});

test('a symlink candidate refuses without traversing its destination', () => {
  const f = fixture();
  try {
    fs.rmSync(f.stale[0]);
    fs.symlinkSync(f.foreign,f.stale[0]);
    assert.throws(() => f.plan(), /symlink/);
    assert.equal(fs.existsSync(f.foreign),true);
    assert.equal(fs.existsSync(f.stale[1]),true);
  } finally { f.cleanup(); }
});
