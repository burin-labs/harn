import fs from 'node:fs';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';

/** Real Git ancestry and release tags; policy files stay outside the producer commit. */
export function consumerPairingRepositories(directory, {required=true}={}) {
  const policy = path.join(directory, 'policy');
  const remote = path.join(directory, 'source.git');
  const consumer = path.join(directory, 'consumer');
  const environment = {...process.env, GIT_AUTHOR_DATE:'2026-11-01T00:00:00Z',
    GIT_COMMITTER_DATE:'2026-11-01T00:00:00Z'};
  const git = (cwd, args, input) => execFileSync('git', ['-c', 'user.name=Fixture',
    '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', ...args],
    {cwd, env:environment, input, encoding:'utf8'}).trim();
  fs.mkdirSync(policy);
  git(policy, ['init', '-q', '-b', 'main']);
  fs.writeFileSync(path.join(policy, 'Cargo.toml'), '[workspace.package]\nversion = "0.10.201"\n');
  git(policy, ['add', 'Cargo.toml']);
  git(policy, ['commit', '-qm', 'Published source']);
  git(policy, ['tag', 'v0.10.201']);
  const previous=git(policy,['rev-parse','HEAD']);
  fs.writeFileSync(path.join(policy, 'Cargo.toml'), '[workspace.package]\nversion = "0.10.202"\n');
  git(policy, ['add', 'Cargo.toml']);
  git(policy, ['commit', '-qm', required?'Consumer adaptation\n\nPairs-with: consumer#8':'Independent source change']);
  git(policy, ['commit', '--allow-empty', '-qm', 'Release v0.10.202']);
  const sourceRevision = git(policy, ['rev-parse', 'HEAD']);
  git(policy, ['tag', 'v0.10.202']);
  // A later publication must not change this older source's range boundary.
  fs.writeFileSync(path.join(policy, 'Cargo.toml'), '[workspace.package]\nversion = "0.10.203"\n');
  git(policy, ['add', 'Cargo.toml']);
  git(policy, ['commit', '-qm', 'Later published source']);
  git(policy, ['tag', 'v0.10.203']);
  git(directory, ['clone', '-q', '--bare', policy, remote]);
  git(policy, ['remote', 'add', 'origin', remote]);
  const scripts = fileURLToPath(new URL('../../', import.meta.url));
  for (const part of ['lib', 'ci']) {
    const destination = path.join(policy, 'scripts', part);
    fs.mkdirSync(destination, {recursive:true});
    for (const file of fs.readdirSync(path.join(scripts, part))) {
      if (/\.(mjs|sh|json)$/.test(file)) {
        fs.copyFileSync(path.join(scripts, part, file), path.join(destination, file));
      }
    }
  }
  fs.mkdirSync(consumer);
  git(consumer, ['init', '-q', '-b', 'main']);
  fs.writeFileSync(path.join(consumer, 'component'), 'original adaptation\n');
  git(consumer, ['add', 'component']);
  git(consumer, ['commit', '-qm', 'Required component']);
  const component = git(consumer, ['rev-parse', 'HEAD']);
  git(consumer, ['branch', 'component']);
  fs.writeFileSync(path.join(consumer, 'combined'), 'tested combined adaptation\n');
  git(consumer, ['add', 'combined']);
  git(consumer, ['commit', '-qm', 'Tested containing repair']);
  const tested = git(consumer, ['rev-parse', 'HEAD']);
  git(consumer, ['checkout', '-q', 'component']);
  fs.writeFileSync(path.join(consumer, 'component'), 'later untested adaptation\n');
  git(consumer, ['add', 'component']);
  git(consumer, ['commit', '-qm', 'Component advanced after the test']);
  const advanced = git(consumer, ['rev-parse', 'HEAD']);
  const mergeBase = git(consumer, ['merge-base', advanced, tested]);
  if (mergeBase !== component || advanced === component || tested === component) {
    throw new Error('the fixture must reach genuine divergent Git ancestry');
  }
  const hideRequirement = view => {
    if(view==='replace'){
      const replacement=git(policy,['commit-tree',git(policy,['rev-parse',`${sourceRevision}^{tree}`]),
        '-p',previous],'Substituted source with no consumer requirement\n');
      git(policy,['replace',sourceRevision,replacement]);
    }else{
      fs.writeFileSync(path.join(policy,'.git/info/grafts'),`${sourceRevision} ${previous}\n`);
    }
    if(git(policy,['log','--format=%B',`v0.10.201..${sourceRevision}`]).includes('consumer#8')){
      throw new Error('the source-view falsifier must actually hide the required component');
    }
  };
  const clearViews = () => {
    if(fs.existsSync(path.join(policy,'.git/refs/replace',sourceRevision))) git(policy,['replace','-d',sourceRevision]);
    fs.rmSync(path.join(policy,'.git/info/grafts'),{force:true});
  };
  return {policy, remote, previous, sourceRevision, component, tested, advanced, mergeBase,hideRequirement,clearViews,
    sourceGit:(args,input)=>git(policy,args,input)};
}
