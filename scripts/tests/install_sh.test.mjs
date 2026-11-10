import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, symlinkSync, existsSync, readdirSync, readlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';

const installer = fileURLToPath(new URL('../../install.sh', import.meta.url));
const legalFiles = ['LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt'];
const targets = [
  ['Darwin', 'arm64', 'aarch64-apple-darwin'],
  ['Darwin', 'x86_64', 'x86_64-apple-darwin'],
  ['Linux', 'aarch64', 'aarch64-unknown-linux-gnu'],
  ['Linux', 'x86_64', 'x86_64-unknown-linux-gnu'],
];

function fixture(target, invalid, existing = false, badChecksum = false, invalidDestination, options = {}) {
  const root = mkdtempSync(join(tmpdir(), 'harn-installed-legal-'));
  try {
    const [os, arch, triple] = target;
    const paths = Object.fromEntries(['archive', 'shim', 'home', 'tmp'].map(name => [name, join(root, name)]));
    for (const path of Object.values(paths)) mkdirSync(path);
    const destination = join(root, options.directoryName ?? 'custom install', 'bin');
    const { pathEntry = 'missing', installVariable = 'HARN_INSTALL_DIR', noModifyPath = true } = options;
    if (pathEntry === 'shadowed') writeFileSync(join(paths.shim, 'harn'), '#!/bin/sh\necho old harn\n', { mode: 0o755 });
    if (pathEntry === 'alias') symlinkSync(join(destination, 'harn'), join(paths.shim, 'harn'));
    const binary = Buffer.from('synthetic binary, never executed\n');
    writeFileSync(join(paths.archive, 'harn'), binary);
    for (const alias of ['harn-dap', 'harn-lsp']) symlinkSync('harn', join(paths.archive, alias));
    const legalBytes = new Map(legalFiles.map(name => [name, Buffer.from(`Exact ${name} text\nCopyright fixture\n`)]));
    for (const [name, bytes] of legalBytes) writeFileSync(join(paths.archive, name), bytes);
    if (invalid) {
      const path = join(paths.archive, invalid.name);
      rmSync(path);
      if (invalid.kind === 'empty') writeFileSync(path, '');
      if (invalid.kind === 'directory') mkdirSync(path);
      if (invalid.kind === 'symlink') symlinkSync(invalid.name === 'harn' ? 'LICENSE-MIT' : 'harn', path);
    }
    const archive = join(root, `harn-${triple}.tar.xz`);
    const tar = spawnSync('tar', ['-cJf', archive, '-C', paths.archive, '.'], { encoding: 'utf8' });
    assert.equal(tar.status, 0, tar.stderr);
    const digest = createHash('sha256').update(readFileSync(archive)).digest('hex');
    const checksums = join(root, 'SHA256SUMS');
    writeFileSync(checksums, `${badChecksum ? '0'.repeat(64) : digest}  harn-${triple}.tar.xz\n`);
    writeFileSync(join(paths.shim, 'uname'), '#!/bin/sh\ncase "$1" in -s) echo "$FIXTURE_OS";; -m) echo "$FIXTURE_ARCH";; *) exit 1;; esac\n', { mode: 0o755 });
    const downloader = options.downloader ?? 'curl';
    // No network-capable command is reached. The real installer still verifies
    // the real archive digest and installs its payload and license material.
    writeFileSync(join(paths.shim, downloader), `#!/bin/sh
dest=""
url=""
agent=""
method=GET
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output|--output-document) shift; dest="$1";;
    --output-document=*) dest="\${1#*=}";;
    --user-agent) shift; agent="$1";;
    --user-agent=*) agent="\${1#*=}";;
    --head|--spider) method=HEAD;;
    https://*) url="$1";;
  esac
  shift
done
printf '%s|%s|%s\\n' "$method" "$url" "$agent" >> "$FIXTURE_REQUESTS"
${options.requireIdentity ? 'test "$agent" = harn-install || exit 22' : ''}
case "$url" in
  https://github.com/burin-labs/harn/releases/latest)
    case "$FIXTURE_LATEST" in
      fallback|failed-api) printf '%s' "$url"; exit 22;;
      unchanged) printf '%s' "$url";;
      redirect)
        if test "$FIXTURE_DOWNLOADER" = curl; then
          printf '%s/tag/v0.10.999' "https://github.com/burin-labs/harn/releases"
        else
          printf '  Location: https://github.com/burin-labs/harn/releases/tag/v0.10.999\\r\\n' >&2
        fi;;
      *) exit 91;;
    esac;;
  https://api.github.com/repos/burin-labs/harn/releases/latest)
    test "$method" = GET || exit 91
    test "$FIXTURE_LATEST" != failed-api || exit 22
    printf '%s\\n' '{"tag_name":"v0.10.999"}' > "$dest";;
  "$FIXTURE_BASE/harn-$FIXTURE_TARGET.tar.xz") /bin/cp "$FIXTURE_ARCHIVE" "$dest";;
  "$FIXTURE_BASE/SHA256SUMS") /bin/cp "$FIXTURE_CHECKSUMS" "$dest";;
  *) exit 91;;
esac
`, { mode: 0o755 });
    if (downloader === 'wget') {
      // Hide the host's curl so the installer actually selects its wget path.
      for (const tool of ['awk', 'grep', 'head', 'install', 'ln', 'mkdir', 'mktemp', 'rm', 'sed', 'sort', 'tar', 'tr', 'xz', 'sha256sum', 'shasum']) {
        const found = spawnSync('/usr/bin/which', [tool], { encoding: 'utf8' });
        if (found.status === 0) symlinkSync(found.stdout.trim(), join(paths.shim, tool));
      }
    }
    const oldBinary = Buffer.from('previous installed binary\n');
    if (existing) {
      mkdirSync(join(destination, 'harn-licenses'), { recursive: true });
      writeFileSync(join(destination, 'harn'), oldBinary);
      for (const alias of ['harn-dap', 'harn-lsp']) {
        if (existing === 'legacy') writeFileSync(join(destination, alias), oldBinary);
        else symlinkSync('harn', join(destination, alias));
      }
      for (const name of legalFiles) writeFileSync(join(destination, 'harn-licenses', name), `previous ${name}\n`);
    }
    const externalSentinel = join(root, 'external-sentinel');
    const externalTarget = invalidDestination?.directoryTarget ? join(root, 'external-directory') : externalSentinel;
    if (invalidDestination) {
      writeFileSync(externalSentinel, 'external bytes must survive\n');
      if (invalidDestination.directoryTarget) mkdirSync(externalTarget);
      const path = invalidDestination.payload
        ? join(destination, invalidDestination.name)
        : join(destination, 'harn-licenses', invalidDestination.name);
      rmSync(path);
      if (invalidDestination.kind === 'symlink') symlinkSync(externalTarget, path);
      else {
        mkdirSync(path);
        writeFileSync(join(path, 'retained-child'), 'previous directory child\n');
      }
    }
    const requests = join(root, 'requests');
    const assertInvalidDestination = path => {
      if (invalidDestination.kind === 'symlink') assert.equal(readlinkSync(path), externalTarget);
      else assert.equal(readFileSync(join(path, 'retained-child'), 'utf8'), 'previous directory child\n');
    };
    const result = spawnSync('/bin/sh', [installer], {
      encoding: 'utf8', timeout: 15_000,
      env: {
        PATH: `${pathEntry === 'installed' ? `${destination}:` : ''}${paths.shim}${downloader === 'wget' ? '' : ':/usr/bin:/bin'}`, HOME: paths.home, TMPDIR: paths.tmp,
        [installVariable]: destination, HARN_VERSION: options.latest ? '' : 'v0.10.999', HARN_NO_MODIFY_PATH: noModifyPath ? '1' : '0',
        FIXTURE_OS: os, FIXTURE_ARCH: arch, FIXTURE_TARGET: triple,
        FIXTURE_LATEST: options.latest ?? '', FIXTURE_DOWNLOADER: downloader,
        FIXTURE_ARCHIVE: archive, FIXTURE_CHECKSUMS: checksums, FIXTURE_REQUESTS: requests,
        FIXTURE_BASE: 'https://github.com/burin-labs/harn/releases/download/v0.10.999',
      },
    });
    assert.equal(result.error, undefined);
    const calls = readFileSync(requests, 'utf8').trim().split('\n').map(line => line.split('|'));
    const expectedCalls = !options.latest ? 2 : options.latest === 'redirect' ? 3 : options.latest === 'failed-api' ? 2 : 4;
    assert.equal(calls.length, expectedCalls, result.stderr);
    if (options.requireIdentity) assert.ok(calls.every(call => call[2] === 'harn-install'));
    if (options.latest) {
      assert.deepEqual(calls[0].slice(0, 2), ['HEAD', 'https://github.com/burin-labs/harn/releases/latest']);
      if (options.latest !== 'redirect') {
        assert.deepEqual(calls[1].slice(0, 2), ['GET', 'https://api.github.com/repos/burin-labs/harn/releases/latest']);
      }
    }
    assert.deepEqual(readdirSync(paths.tmp), [], 'download/extraction directory must be removed');
    if (options.latest === 'failed-api') {
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /could not determine latest release tag/);
      assert.equal(existsSync(destination), false, 'failed discovery must not create an installation');
      return result;
    }
    if (invalid || badChecksum || invalidDestination) {
      assert.notEqual(result.status, 0, 'invalid payload or destination must refuse');
      if (invalidDestination) assert.match(result.stderr, /must (not be a symbolic link|be a regular file|point to)/);
      else assert.match(result.stderr, invalid ? new RegExp(`missing or invalid ${invalid.name.replaceAll('.', '\\.')}`) : /checksum mismatch/);
      if (existing) {
        if (invalidDestination?.payload && invalidDestination.name === 'harn') assertInvalidDestination(join(destination, 'harn'));
        else assert.deepEqual(readFileSync(join(destination, 'harn')), oldBinary);
        for (const alias of ['harn-dap', 'harn-lsp']) {
          if (invalidDestination?.payload && invalidDestination.name === alias) assertInvalidDestination(join(destination, alias));
          else assert.equal(readlinkSync(join(destination, alias)), 'harn');
        }
        for (const name of legalFiles) {
          const path = join(destination, 'harn-licenses', name);
          if (name === invalidDestination?.name && !invalidDestination.payload) assertInvalidDestination(path);
          else assert.equal(readFileSync(path, 'utf8'), `previous ${name}\n`);
        }
        if (invalidDestination) assert.equal(readFileSync(externalSentinel, 'utf8'), 'external bytes must survive\n');
        if (invalidDestination?.directoryTarget) assert.deepEqual(readdirSync(externalTarget), [], 'alias installation must not write into an external directory');
      } else assert.equal(existsSync(destination), false, 'refusal must not create an installation');
    } else {
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(readFileSync(join(destination, 'harn')), binary);
      for (const [name, bytes] of legalBytes) assert.deepEqual(readFileSync(join(destination, 'harn-licenses', name)), bytes);
      for (const alias of ['harn-dap', 'harn-lsp']) assert.equal(readlinkSync(join(destination, alias)), 'harn');
      for (const name of legalFiles) assert.equal(existsSync(join(destination, name)), false, 'shared binary directory has no generic legal filenames');
      if (options.verifyPathExport) {
        const exportLine = result.stdout.match(/^\s+export PATH=.+$/m)?.[0];
        assert.ok(exportLine, 'installer must print its PATH command');
        const script = join(root, 'apply-path.sh');
        writeFileSync(script, `${exportLine.trim()}\nprintf '%s\\n' "$PATH"\n`);
        const applied = spawnSync('/bin/sh', [script], { encoding: 'utf8', env: { PATH: '/usr/bin:/bin' } });
        assert.equal(applied.status, 0, applied.stderr);
        assert.equal(applied.stderr, '');
        assert.equal(applied.stdout, `${destination}:/usr/bin:/bin\n`);
      }
    }
    return { ...result, destination };
  } finally { rmSync(root, { recursive: true, force: true }); }
}

test('real installer falls back to the release API after a failed latest redirect', () => {
  fixture(targets[3], undefined, false, false, undefined, { latest: 'fallback' });
});

for (const downloader of ['curl', 'wget']) {
  test(`real installer identifies ${downloader} archive and checksum requests`, () => {
    fixture(targets[3], undefined, false, false, undefined, { downloader, requireIdentity: true });
  });
  for (const latest of ['redirect', 'fallback', 'unchanged', 'failed-api']) {
    test(`real installer selects ${downloader} latest discovery with ${latest}`, () => {
      fixture(targets[3], undefined, false, false, undefined, { downloader, latest, requireIdentity: true });
    });
  }
}

for (const installVariable of ['HARN_INSTALL_DIR', 'XDG_BIN_DIR']) {
  test(`real installer explains a missing PATH entry for ${installVariable}`, () => {
    const result = fixture(targets[0], undefined, false, false, undefined, { installVariable, noModifyPath: false });
    assert.ok(result.stdout.includes(`Add ${result.destination} to your PATH:`));
    assert.ok(result.stdout.includes(`export PATH="${result.destination}:$PATH"`));
    assert.doesNotMatch(result.stderr, /another version/);
  });
}
test('real installer PATH command preserves shell metacharacters literally', () => {
  fixture(targets[0], undefined, false, false, undefined, {
    noModifyPath: false, verifyPathExport: true, directoryName: 'custom "quotes" $dollar `ticks` \\slash',
  });
});
test('real installer reports an executable that shadows the new installation', () => {
  const result = fixture(targets[0], undefined, false, false, undefined, { pathEntry: 'shadowed', noModifyPath: false });
  assert.match(result.stderr, /another version may be ahead of it on PATH/);
  assert.ok(result.stdout.includes(`Add ${result.destination} to your PATH:`));
});
for (const pathEntry of ['installed', 'alias']) {
  test(`real installer does not report its own reachable executable as shadowed: ${pathEntry}`, () => {
    const result = fixture(targets[0], undefined, false, false, undefined, { pathEntry, noModifyPath: false });
    assert.doesNotMatch(result.stderr, /another version/);
    if (pathEntry === 'installed') assert.doesNotMatch(result.stdout, /Add .* to your PATH:/);
  });
}
for (const pathEntry of ['missing', 'shadowed']) {
  test(`real installer honors suppressed PATH guidance: ${pathEntry}`, () => {
    const result = fixture(targets[0], undefined, false, false, undefined, { pathEntry });
    assert.doesNotMatch(result.stdout, /Add .* to your PATH:/);
    if (pathEntry === 'missing') assert.doesNotMatch(result.stderr, /another version/);
    else assert.match(result.stderr, /another version may be ahead of it on PATH/);
  });
}

for (const target of targets) {
  test(`real installer retains exact legal payload and aliases: ${target[2]}`, () => fixture(target));
  test(`real installer updates valid existing legal payload: ${target[2]}`, () => fixture(target, undefined, true));
}
for (const name of legalFiles) {
  for (const kind of ['missing', 'empty', 'directory', 'symlink']) {
    for (const target of targets) {
      for (const existing of [false, true]) {
        test(`real installer refuses ${kind} ${name}, existing=${existing}: ${target[2]}`, () => fixture(target, { name, kind }, existing));
      }
    }
  }
}
for (const name of ['harn', 'harn-dap', 'harn-lsp']) {
  test(`real installer preflights existing payload directory ${name}`, () => fixture(targets[0], undefined, true, false, { name, kind: 'directory', payload: true }));
}
test('real installer preflights existing payload symlink harn', () => fixture(targets[0], undefined, true, false, { name: 'harn', kind: 'symlink', payload: true }));
for (const name of ['harn-dap', 'harn-lsp']) {
  for (const directoryTarget of [false, true]) {
    test(`real installer refuses external alias symlink ${name}, directory=${directoryTarget}`, () => fixture(targets[0], undefined, true, false, { name, kind: 'symlink', payload: true, directoryTarget }));
  }
}
for (const kind of ['missing', 'empty', 'directory', 'symlink']) {
  test(`real installer refuses ${kind} executable before changing legal files`, () => fixture(targets[0], { name: 'harn', kind }, true));
}
test('real installer checks the downloaded archive digest before installation', () => fixture(targets[0], undefined, false, true));
test('real installer updates legacy executable alias copies', () => fixture(targets[0], undefined, 'legacy'));
for (const name of legalFiles) {
  for (const kind of ['symlink', 'directory']) {
    test(`real installer preflights existing ${kind} destination ${name}`, () => fixture(targets[0], undefined, true, false, { name, kind }));
  }
}

test('every installer names the first xz release the same way', () => {
  const read = (file) => readFileSync(new URL(`../../${file}`, import.meta.url), 'utf8');
  const shell = (file) => read(file).match(/^XZ_FIRST_RELEASE=(\d+\.\d+\.\d+)$/m)?.[1];
  const harn = read('crates/harn-stdlib/src/stdlib/runtime/install.harn').match(/^const XZ_FIRST_RELEASE = "(\d+\.\d+\.\d+)"$/m)?.[1];
  const rust = read('crates/harn-cli/src/commands/upgrade.rs').match(/^const XZ_FIRST_RELEASE: \[u64; 3\] = \[(\d+), (\d+), (\d+)\];$/m)?.slice(1).join('.');
  assert.ok(harn, 'install.harn declares XZ_FIRST_RELEASE');
  assert.equal(shell('install.sh'), harn);
  assert.equal(shell('scripts/bootstrap-harn.sh'), harn);
  assert.equal(rust, harn);
});
