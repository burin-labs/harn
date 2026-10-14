#!/usr/bin/env node
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const lane = path.resolve(__dirname, '../ci/run_rust_lint_lane.sh');
const real = process.argv.includes('--real');
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-lint-timings-'));
try {
  const bin = path.join(root, 'bin');
  fs.mkdirSync(bin);
  // This process fixture exercises the real entrypoint's report lifecycle.
  // Cargo's HTML format and actual compiler invocation are checked separately.
  fs.writeFileSync(path.join(bin, 'cargo'), `#!/usr/bin/env node
const fs=require('node:fs'); const path=require('node:path');
const args=process.argv.slice(2);
fs.appendFileSync(process.env.CALLS,JSON.stringify(args)+'\\n');
if(args[0]==='clean') process.exit(0);
if(args[0]==='metadata') { console.log(JSON.stringify({target_directory:process.env.TARGET})); process.exit(0); }
if(JSON.stringify(args)!==JSON.stringify(['clippy','--workspace','--all-targets','--timings','--','-D','warnings'])) process.exit(89);
const report=path.join(process.env.TARGET,'cargo-timings/cargo-timing.html');
if(fs.existsSync(report)) process.exit(88);
if(process.env.MODE!=='missing') {
 fs.mkdirSync(path.dirname(report),{recursive:true});
 fs.writeFileSync(report,process.env.MODE==='oversized'?'x'.repeat(10485761):'<html>actual new invocation</html>');
}
if(process.env.MODE==='changed') fs.appendFileSync('source.txt','changed');
if(process.env.MODE==='advanced') {
 const {spawnSync}=require('node:child_process');
 fs.appendFileSync('source.txt','advanced');
 for(const args of [['add','source.txt'],['-c','user.name=Fixture','-c','user.email=fixture@example.invalid','-c','maintenance.auto=false','commit','-qm','advance']]) {
  const child=spawnSync('git',args,{encoding:'utf8'});
  if(child.status!==0) process.exit(87);
 }
}
process.exit(process.env.MODE==='failed'?7:0);
`);
  fs.chmodSync(path.join(bin, 'cargo'), 0o755);
  function command(program, args, cwd, env = process.env) {
    return spawnSync(program, args, {cwd, env, encoding: 'utf8'});
  }
  for (const mode of real ? ['success'] : ['success', 'failed', 'missing', 'oversized', 'changed', 'advanced', 'wrong-source', 'stale-output']) {
    const cwd = path.join(root, mode);
    fs.mkdirSync(cwd);
    const git = (args) => {
      const child = command('git', ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'maintenance.auto=false', ...args], cwd);
      assert.equal(child.status, 0, child.stderr);
      return child.stdout.trim();
    };
    git(['init', '-q']);
    fs.writeFileSync(path.join(cwd, 'source.txt'), 'fixture');
    if (real) {
      fs.mkdirSync(path.join(cwd, 'src'));
      fs.writeFileSync(path.join(cwd, 'Cargo.toml'), '[package]\nname="harn-timing-fixture"\nversion="0.1.0"\nedition="2021"\n[workspace]\n');
      fs.writeFileSync(path.join(cwd, 'src/main.rs'), 'fn main() {}\n');
      git(['add', 'Cargo.toml', 'src/main.rs']);
    }
    git(['add', 'source.txt']);
    git(['commit', '-qm', 'fixture']);
    const source = git(['rev-parse', 'HEAD']);
    const target = path.join(root, `target-${mode}`);
    fs.mkdirSync(path.join(target, 'cargo-timings'), {recursive:true});
    fs.writeFileSync(path.join(target, 'cargo-timings/cargo-timing.html'), 'stale restored report');
    const output = path.join(root, `output-${mode}`);
    if (mode === 'stale-output') {
      fs.mkdirSync(output);
      fs.writeFileSync(path.join(output, 'compiler-sample.json'), JSON.stringify({status: 'MEASURED', sourceCommit: '0'.repeat(40)}));
    }
    const calls = path.join(root, `calls-${mode}`);
    const child = command('bash', [lane, '--leg', 'workspace'], cwd, {
      ...process.env, PATH:real?process.env.PATH:`${bin}${path.delimiter}${process.env.PATH}`, MODE:mode,
      CARGO_TARGET_DIR:target,
      TARGET:target, CALLS:calls, HARN_LINT_TIMINGS_DIR:output,
      HARN_LINT_SOURCE_SHA:mode==='wrong-source'?'0'.repeat(40):source,
    });
    if (mode==='success' || mode==='failed') {
      assert.equal(child.status, mode==='success'?0:7, child.stderr);
      const receipt=JSON.parse(fs.readFileSync(path.join(output,'receipt.json'),'utf8'));
      assert.equal(receipt.sourceCommit, source);
      assert.equal(receipt.sourceTree, git(['rev-parse','HEAD^{tree}']));
      assert.equal(receipt.exitCode, child.status);
      assert.equal(receipt.reportGitBlob, git(['hash-object',path.join(output,'cargo-timing.html')]));
      assert.equal(receipt.reportBytes, fs.statSync(path.join(output,'cargo-timing.html')).size);
      assert(Number.isFinite(Date.parse(receipt.compileStartedAt)));
      assert(Date.parse(receipt.compileFinishedAt) >= Date.parse(receipt.compileStartedAt));
      assert.match(receipt.compilerSampleReceipt, /^compiler-observation\.[A-Za-z0-9]+\/compiler-sample\.json$/);
      const diagnostic = JSON.parse(fs.readFileSync(path.join(output, receipt.compilerSampleReceipt), 'utf8'));
      assert.equal(diagnostic.sourceCommit, source);
      assert.equal(diagnostic.sourceTree, receipt.sourceTree);
      assert.equal(diagnostic.status, 'UNMEASURED', 'mock compiler must not supply real symbol evidence');
      assert(diagnostic.refusal && ['execution_identity','settlement'].includes(diagnostic.refusal.stage), 'diagnostic absence needs its measured stage');
      assert(['unsupported_host','unsupported_or_invalid','receipt_missing','interrupted'].includes(diagnostic.refusal.cause), 'diagnostic absence needs a closed cause');
      if (process.platform !== 'darwin') assert.equal(diagnostic.observerExitCode, null);
      if (real) {
        const report=fs.readFileSync(path.join(output,'cargo-timing.html'),'utf8');
        assert(report.includes('harn-timing-fixture'), 'actual Cargo report did not include the compiled fixture');
      } else {
        const actual=fs.readFileSync(calls,'utf8').trim().split('\n').map(JSON.parse);
        assert.equal(actual.filter(args=>args[0]==='clippy').length,1);
      }
    } else {
      assert.equal(child.status, 1, child.stderr);
      assert(!fs.existsSync(path.join(output,'receipt.json')));
      assert(child.stderr.includes({missing:'did not produce',oversized:'exceeds 10 MiB',changed:'source changed',advanced:'source changed', 'wrong-source':'requested clean commit', 'stale-output':'output already exists'}[mode]),child.stderr);
      if(mode==='advanced') {
        assert.notEqual(git(['rev-parse','HEAD']),source);
        assert.equal(git(['status','--porcelain']),'');
      }
    }
    console.log(`rust_lint_timings_test: ${real?'actual Cargo ':''}${mode} reached`);
  }
  const sampling = require('../ci/macos_compiler_sample.cjs');
  const commandProbe = path.join(root, 'command-probe.cjs');
  fs.writeFileSync(commandProbe, `const {hostCommand}=require(${JSON.stringify(path.resolve(__dirname, '../ci/macos_compiler_sample.cjs'))});
const selected=process.argv[2];
const fixtures={
 positive:[process.execPath,['-e','process.stdout.write("measured")'],32768,3000],
 unavailable:[${JSON.stringify(path.join(root, 'absent-command'))},[],32768,3000],
 empty:[process.execPath,['-e','process.exit(0)'],32768,3000],
 failed:[process.execPath,['-e','process.stderr.write("PRIVATE HOST PATH AND PAYLOAD");process.exit(7)'],32768,3000],
 clipped:[process.execPath,['-e','process.stdout.write(JSON.stringify({ok:true})+" ".repeat(4096))'],32,3000],
 timeout:[process.execPath,['-e','process.kill(process.pid,"SIGSTOP")'],32768,50],
};
const [program,args,limit,timeout]=fixtures[selected];
hostCommand(program,args,limit,'sample_command',timeout).result.then(
 output=>console.log(JSON.stringify({status:'MEASURED',output})),
 error=>console.log(JSON.stringify({status:'UNMEASURED',stage:error.stage,cause:error.cause,message:error.message})),
);
`);
  for (const [selected, cause] of Object.entries({positive:null,unavailable:'unavailable',empty:'empty_output',failed:'command_failed',clipped:'clipped',timeout:'timed_out'})) {
    const probe = spawnSync(process.execPath, [commandProbe, selected], {encoding:'utf8', timeout:10000, maxBuffer:32768});
    assert.equal(probe.status, 0, probe.stderr);
    const measured = JSON.parse(probe.stdout);
    if (cause === null) assert.deepEqual(measured, {status:'MEASURED', output:'measured'});
    else assert.deepEqual(measured, {status:'UNMEASURED',stage:'sample_command',cause,message:'unmeasured host observation'});
    assert(!probe.stdout.includes('PRIVATE HOST'), 'host errors must never enter the diagnostic');
    console.log(`rust_lint_timings_test: actual observation command ${selected} reached`);
  }
  const stamp = 'Sat Oct 10 01:10:00 2026';
  const row = (pid, parent, executable, start = stamp) => `${pid} ${parent} ${start} ${executable}`;
  const complete = [row(10, 1, '/bin/bash'), row(11, 10, '/bin/cargo'), row(12, 11, '/bin/clippy-driver'), row(13, 1, '/bin/rustc')].join('\n');
  const observed = sampling.census(complete, 10);
  assert.deepEqual(observed.compilers.map(value => value.pid), [12], 'foreign compiler must not be sampled');
  assert.throws(() => sampling.census('', 10), /unmeasured/);
  assert.throws(() => sampling.census('', 10), error => error.stage === 'process_census' && error.cause === 'malformed_row');
  assert.throws(() => sampling.census(complete.replace(row(11, 10, '/bin/cargo'), ''), 10), /unmeasured/);
  assert.throws(() => sampling.census(complete + '\n' + row(12, 11, '/bin/clippy-driver'), 10), /incomplete/);
  assert.throws(() => sampling.census(complete.replace(stamp, 'Sat Oct 10 01:10:01 2026'), 10, observed.owner), /changed/);
  assert.equal(sampling.sameProcess(observed.compilers[0], {...observed.compilers[0], start:'changed'}), false);
  const sampledTarget = { pid: 321, executable: 'clippy-driver' };
  const symbols = 'PRIVATE HOST HEADER\nProcess: clippy-driver [321]\nCall graph:\n  20 rustc_hir_typeck::check_body\n\nTotal number in stack: 20\nPRIVATE BINARY PATH';
  assert.equal(sampling.symbolStacks(symbols, sampledTarget), 'Call graph:\n  20 rustc_hir_typeck::check_body\n');
  assert.throws(() => sampling.symbolStacks(symbols.replace('[321]', '[322]'), sampledTarget), /unmeasured/);
  assert.throws(() => sampling.symbolStacks(symbols.replace('Process: clippy-driver', 'Process: rustc'), sampledTarget), /unmeasured/);
  assert.throws(() => sampling.symbolStacks(symbols.split('Total number')[0], sampledTarget), /unmeasured/);
  assert.throws(() => sampling.symbolStacks(symbols.split('Total number')[0], sampledTarget), error => error.stage === 'sample_parse' && error.cause === 'incomplete_graph');
  assert.throws(() => sampling.symbolStacks(symbols.replace('rustc_hir_typeck', 'sleep'), sampledTarget), /unmeasured/);
  assert.throws(() => sampling.symbolStacks(symbols.replace('  20', 'x'.repeat(131073)), sampledTarget), /unmeasured/);
  // A real process census reaches a known live owned child, without inspecting
  // process argv or environment. The diagnostic classifier must not call an
  // ordinary child a compiler or admit a fabricated/missing owner PID.
  const { spawn } = require('node:child_process');
  const live = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 10000)'], {stdio:'ignore'});
  try {
    const ps = spawnSync('/bin/ps', ['-axo', 'pid=,ppid=,lstart=,comm='], {encoding:'utf8', timeout:3000, maxBuffer:1048576});
    if (process.platform === 'darwin') {
      assert.equal(ps.status, 0, ps.stderr);
      assert(ps.stdout.split('\n').some(line => line.trim().startsWith(`${live.pid} `)), 'known live child omitted');
      assert.equal(sampling.census(ps.stdout, process.pid).compilers.length, 0);
      assert.throws(() => sampling.census(ps.stdout, 2147483647), /incomplete/);
    }
  } finally { live.kill('SIGTERM'); }
  const lifecycle = path.join(root, 'sample-lifecycle.cjs');
  fs.writeFileSync(lifecycle, `const {spawn}=require('node:child_process');
const fs=require('node:fs'),path=require('node:path');
const output=process.argv[3];fs.mkdirSync(output);
const child=spawn(process.execPath,[process.argv[2],String(process.pid),output,'123','1','a'.repeat(40),'b'.repeat(40)],{stdio:'ignore'});
const began=Date.now();setTimeout(()=>child.kill('SIGTERM'),400);
child.on('close',code=>{
 try { const receipt=JSON.parse(fs.readFileSync(path.join(output,'compiler-sample.json'),'utf8'));
 console.log(JSON.stringify({code,elapsed:Date.now()-began,receipt,owner:process.pid})); }
 catch {process.exitCode=1;}
});
`);
  const settlement = spawnSync(process.execPath, [lifecycle, path.resolve(__dirname, '../ci/macos_compiler_sample.cjs'), path.join(root, 'live-sample')],
    {encoding:'utf8', timeout:10000, maxBuffer:32768});
  assert.equal(settlement.status, 0, settlement.stderr);
  const settled = JSON.parse(settlement.stdout);
  assert.equal(settled.code, 0);
  assert(settled.elapsed < 5000, 'owned sampler did not settle after interruption');
  assert.equal(settled.receipt.status, 'UNMEASURED');
  assert.deepEqual(settled.receipt.samples, []);
  if (process.platform === 'darwin') assert.deepEqual(settled.receipt.refusal, {stage:'settlement',cause:'interrupted'});
  else assert.deepEqual(settled.receipt.refusal, {stage:'execution_identity',cause:'unsupported_or_invalid'});
  if (process.platform === 'darwin') assert.equal(settled.receipt.owner.pid, settled.owner);
  console.log('rust_lint_timings_test: compiler identity, foreign owner, clipped symbols and actual live-child controls reached');
} finally {
  fs.rmSync(root, {recursive:true, force:true});
}
