const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

// This owner measures Linux's actual GNU time, not a success-shaped time stub.
const linux = process.platform === 'linux';
const owner = path.resolve(__dirname, '../ci/warm_rust_cache.sh');
function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-workspace-phase-'));
  const bin = path.join(root, 'bin');
  const deps = path.join(root, 'target/debug/deps');
  fs.mkdirSync(bin);
  fs.mkdirSync(deps, { recursive: true });
  fs.mkdirSync(path.join(root, 'scripts/ci'), { recursive: true });
  fs.writeFileSync(path.join(root, 'scripts/ci/host_bound_rust_test_filter.sh'), '#!/bin/sh\nprintf "test(host_bound)\\n"\n', { mode: 0o755 });
  const command = `#!${process.execPath}
const fs=require('node:fs');
const path=require('node:path');
const args=process.argv.slice(2);
const name=path.basename(process.argv[1]);
fs.appendFileSync(process.env.PROBE_CALLS,JSON.stringify({name,args})+'\\n');
if(name==='cargo' && args[0]==='metadata') {
  console.log(JSON.stringify({target_directory:process.env.PROBE_TARGET}));
} else {
  let phase=name==='cargo'?'harn-build':args.at(-1).includes('not (')?'workspace-tests':'security-tests';
  if(process.env.PROBE_FAIL===phase) process.exit(23);
  if(process.env.PROBE_INTERRUPT===phase) {
    process.stdout.write('probe-compiler-active\\n');
    fs.readSync(0,Buffer.alloc(1),0,1,null);
    process.exit(99);
  }
  const allocation=Buffer.alloc(8*1024*1024,1);
  if(allocation[0]!==1) process.exit(99);
}

`;
  for (const name of ['cargo', 'cargo-nextest']) fs.writeFileSync(path.join(bin, name), command, { mode: 0o755 });
  fs.writeFileSync(path.join(deps, 'test-binary'), 'executable fixture', { mode: 0o755 });
  fs.writeFileSync(path.join(deps, 'libfixture.rlib'), 'retained library');
  return { root, deps, env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, TMPDIR: root,
    PROBE_TARGET: path.join(root, 'target'), PROBE_CALLS: path.join(root, 'calls.jsonl') } };
}

test('the real warm owner measures all three exact compiler phases and retains cache pruning', { skip: !linux }, () => {
  const f = fixture();
  try {
    const run = spawnSync('bash', [owner], { cwd: f.root, env: f.env, encoding: 'utf8', timeout: 10000 });
    assert.equal(run.status, 0, run.stderr);
    for (const phase of ['harn-build', 'workspace-tests', 'security-tests']) {
      assert.match(run.stdout, new RegExp(`phase=${phase} state=begin disk_available_kib=[1-9][0-9]* memory_available_kib=[1-9][0-9]*`));
      assert.match(run.stdout, new RegExp(`phase=${phase} state=end compiler_exit=0 elapsed_seconds=[0-9.]+ peak_rss_kib=[1-9][0-9]*`));
    }
    const calls = fs.readFileSync(f.env.PROBE_CALLS, 'utf8').trim().split('\n').map(JSON.parse);
    assert.deepEqual(calls, [
      {name:'cargo', args:['build','--locked','--bin','harn']},
      {name:'cargo-nextest', args:['nextest','run','--locked','--workspace','--profile','ci','--no-run','-E','not (test(host_bound))']},
      {name:'cargo-nextest', args:['nextest','run','--locked','--workspace','--profile','ci','--no-run','-E','(package(harn-vm) and binary(harn_vm)) or (package(harn-hostlib) and binary(harn_hostlib))']},
      {name:'cargo', args:['metadata','--format-version','1','--no-deps']},
    ]);
    assert.equal(fs.existsSync(path.join(f.deps, 'test-binary')), false);
    assert.equal(fs.readFileSync(path.join(f.deps, 'libfixture.rlib'), 'utf8'), 'retained library');
    assert.equal(fs.readdirSync(f.root).some(name => name.startsWith('harn-workspace-warm.')), false);
  } finally { fs.rmSync(f.root, { recursive: true, force: true }); }
});

for (const phase of ['harn-build', 'workspace-tests', 'security-tests']) {
  test(`the real warm owner preserves ${phase} compiler refusal without certifying later phases`, { skip: !linux }, () => {
    const f = fixture();
    try {
      const run = spawnSync('bash', [owner], { cwd: f.root, env: { ...f.env, PROBE_FAIL: phase }, encoding: 'utf8', timeout: 10000 });
      assert.equal(run.status, 23, run.stderr);
      assert.match(run.stdout, new RegExp(`phase=${phase} state=end compiler_exit=23`));
      assert.doesNotMatch(run.stdout, new RegExp(`phase=${phase} state=end compiler_exit=0`));
      assert.equal(fs.existsSync(path.join(f.deps, 'test-binary')), true, 'a failed compile must not prune cache outputs');
      assert.equal(fs.readdirSync(f.root).some(name => name.startsWith('harn-workspace-warm.')), false);
    } finally { fs.rmSync(f.root, { recursive: true, force: true }); }
  });
}

test('successful compiler execution without metrics refuses instead of certifying a phase', { skip: !linux }, () => {
  const f = fixture();
  try {
    const transport = path.join(f.root, 'unreported-time');
    fs.writeFileSync(transport, '#!/bin/sh\nif [ "$1" = "--version" ]; then printf "GNU Time\\n"; exit 0; fi\nwhile [ "$1" != "--" ]; do shift; done\nshift\nexec "$@"\n', {mode:0o755});
    const source = fs.readFileSync(owner, 'utf8');
    assert.equal(source.split('/usr/bin/time').length - 1, 2, 'only the owning time transport is substituted');
    const copiedOwner = path.join(f.root, 'warm-owner.sh');
    fs.writeFileSync(copiedOwner, source.replaceAll('/usr/bin/time', transport));
    const run = spawnSync('bash', [copiedOwner], {cwd:f.root, env:f.env, encoding:'utf8', timeout:10000});
    assert.equal(run.status, 1);
    assert.match(run.stderr, /harn-build compiler metrics are unmeasured/);
    assert.match(run.stdout, /phase=harn-build state=begin/);
    assert.doesNotMatch(run.stdout, /phase=harn-build state=end/);
    const calls = fs.readFileSync(f.env.PROBE_CALLS,'utf8').trim().split('\n').map(JSON.parse);
    assert.deepEqual(calls, [{name:'cargo',args:['build','--locked','--bin','harn']}], 'the compiler actually fired before absent metrics refused');
    assert.equal(fs.existsSync(path.join(f.deps,'test-binary')), true);
  } finally { fs.rmSync(f.root,{recursive:true,force:true}); }
});

test('a killed foreground warm process leaves an actual compiler begin witness without a successful end', { skip: !linux, timeout: 10000 }, async () => {
  const f = fixture();
  let output = '';
  try {
    const child = spawn('bash', [owner], { cwd: f.root, env: { ...f.env, PROBE_INTERRUPT: 'workspace-tests' }, detached: true });
    child.stdout.setEncoding('utf8');
    let interrupted = false;
    child.stdout.on('data', chunk => {
      output += chunk;
      if (!interrupted && output.includes('probe-compiler-active')) {
        interrupted = true;
        process.kill(-child.pid, 'SIGKILL');
      }
    });
    const result = await new Promise((resolve, reject) => {
      child.once('error', reject);
      child.once('close', (code, signal) => resolve({code,signal}));
    });
    assert.equal(interrupted, true, 'the control must reach the actual foreground compiler');
    assert.equal(result.signal, 'SIGKILL');
    assert.match(output, /phase=harn-build state=end compiler_exit=0/);
    assert.match(output, /phase=workspace-tests state=begin/);
    assert.doesNotMatch(output, /phase=workspace-tests state=end/);
    assert.doesNotMatch(output, /phase=security-tests state=begin/);
    assert.equal(fs.existsSync(path.join(f.deps, 'test-binary')), true);
  } finally { fs.rmSync(f.root, {recursive:true,force:true}); }
});
