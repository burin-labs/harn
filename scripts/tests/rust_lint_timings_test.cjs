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
  for (const mode of real ? ['success'] : ['success', 'failed', 'missing', 'oversized', 'changed', 'advanced', 'wrong-source']) {
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
      assert(child.stderr.includes({missing:'did not produce',oversized:'exceeds 10 MiB',changed:'source changed',advanced:'source changed', 'wrong-source':'requested clean commit'}[mode]),child.stderr);
      if(mode==='advanced') {
        assert.notEqual(git(['rev-parse','HEAD']),source);
        assert.equal(git(['status','--porcelain']),'');
      }
    }
    console.log(`rust_lint_timings_test: ${real?'actual Cargo ':''}${mode} reached`);
  }
} finally {
  fs.rmSync(root, {recursive:true, force:true});
}
