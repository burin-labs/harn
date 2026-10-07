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

function fixture(target, invalid, existing = false, badChecksum = false, invalidDestination) {
  const root = mkdtempSync(join(tmpdir(), 'harn-installed-legal-'));
  try {
    const [os, arch, triple] = target;
    const paths = Object.fromEntries(['archive', 'shim', 'home', 'tmp'].map(name => [name, join(root, name)]));
    for (const path of Object.values(paths)) mkdirSync(path);
    const destination = join(root, 'custom install', 'bin');
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
      if (invalid.kind === 'symlink') symlinkSync('harn', path);
    }
    const archive = join(root, `harn-${triple}.tar.gz`);
    const tar = spawnSync('tar', ['-czf', archive, '-C', paths.archive, '.'], { encoding: 'utf8' });
    assert.equal(tar.status, 0, tar.stderr);
    const digest = createHash('sha256').update(readFileSync(archive)).digest('hex');
    const checksums = join(root, 'SHA256SUMS');
    writeFileSync(checksums, `${badChecksum ? '0'.repeat(64) : digest}  harn-${triple}.tar.gz\n`);
    writeFileSync(join(paths.shim, 'uname'), '#!/bin/sh\ncase "$1" in -s) echo "$FIXTURE_OS";; -m) echo "$FIXTURE_ARCH";; *) exit 1;; esac\n', { mode: 0o755 });
    // Only the two exact download URLs are accepted. No network-capable command
    // is reached, while the real installer still verifies the real tar digest.
    writeFileSync(join(paths.shim, 'curl'), `#!/bin/sh
dest=""
while [ "$#" -gt 1 ]; do
  if [ "$1" = "--output" ]; then dest="$2"; shift; fi
  shift
done
printf '%s\\n' "$1" >> "$FIXTURE_REQUESTS"
case "$1" in
  "$FIXTURE_BASE/harn-$FIXTURE_TARGET.tar.gz") /bin/cp "$FIXTURE_ARCHIVE" "$dest";;
  "$FIXTURE_BASE/SHA256SUMS") /bin/cp "$FIXTURE_CHECKSUMS" "$dest";;
  *) exit 91;;
esac
`, { mode: 0o755 });
    const oldBinary = Buffer.from('previous installed binary\n');
    if (existing) {
      mkdirSync(join(destination, 'harn-licenses'), { recursive: true });
      writeFileSync(join(destination, 'harn'), oldBinary);
      for (const alias of ['harn-dap', 'harn-lsp']) symlinkSync('harn', join(destination, alias));
      for (const name of legalFiles) writeFileSync(join(destination, 'harn-licenses', name), `previous ${name}\n`);
    }
    const externalSentinel = join(root, 'external-sentinel');
    if (invalidDestination) {
      writeFileSync(externalSentinel, 'external bytes must survive\n');
      const path = join(destination, 'harn-licenses', invalidDestination.name);
      rmSync(path);
      if (invalidDestination.kind === 'symlink') symlinkSync(externalSentinel, path);
      else {
        mkdirSync(path);
        writeFileSync(join(path, 'retained-child'), 'previous directory child\n');
      }
    }
    const requests = join(root, 'requests');
    const result = spawnSync('/bin/sh', [installer], {
      encoding: 'utf8', timeout: 15_000,
      env: {
        PATH: `${paths.shim}:/usr/bin:/bin`, HOME: paths.home, TMPDIR: paths.tmp,
        HARN_INSTALL_DIR: destination, HARN_VERSION: 'v0.10.999', HARN_NO_MODIFY_PATH: '1',
        FIXTURE_OS: os, FIXTURE_ARCH: arch, FIXTURE_TARGET: triple,
        FIXTURE_ARCHIVE: archive, FIXTURE_CHECKSUMS: checksums, FIXTURE_REQUESTS: requests,
        FIXTURE_BASE: 'https://github.com/burin-labs/harn/releases/download/v0.10.999',
      },
    });
    assert.equal(result.error, undefined);
    assert.equal(readFileSync(requests, 'utf8').trim().split('\n').length, 2);
    assert.deepEqual(readdirSync(paths.tmp), [], 'download/extraction directory must be removed');
    if (invalid || badChecksum || invalidDestination) {
      assert.notEqual(result.status, 0, 'invalid archive must refuse');
      if (invalidDestination) assert.match(result.stderr, /must (not be a symbolic link|be a regular file)/);
      else assert.match(result.stderr, invalid ? new RegExp(`missing or invalid ${invalid.name.replaceAll('.', '\\.')}`) : /checksum mismatch/);
      if (existing) {
        assert.deepEqual(readFileSync(join(destination, 'harn')), oldBinary);
        for (const alias of ['harn-dap', 'harn-lsp']) assert.equal(readlinkSync(join(destination, alias)), 'harn');
        for (const name of legalFiles) {
          const path = join(destination, 'harn-licenses', name);
          if (name === invalidDestination?.name) {
            if (invalidDestination.kind === 'symlink') assert.equal(readlinkSync(path), externalSentinel);
            else assert.equal(readFileSync(join(path, 'retained-child'), 'utf8'), 'previous directory child\n');
          } else assert.equal(readFileSync(path, 'utf8'), `previous ${name}\n`);
        }
        if (invalidDestination) assert.equal(readFileSync(externalSentinel, 'utf8'), 'external bytes must survive\n');
      } else assert.equal(existsSync(destination), false, 'refusal must not create an installation');
    } else {
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(readFileSync(join(destination, 'harn')), binary);
      for (const [name, bytes] of legalBytes) assert.deepEqual(readFileSync(join(destination, 'harn-licenses', name)), bytes);
      for (const alias of ['harn-dap', 'harn-lsp']) assert.equal(readlinkSync(join(destination, alias)), 'harn');
      for (const name of legalFiles) assert.equal(existsSync(join(destination, name)), false, 'shared binary directory has no generic legal filenames');
    }
  } finally { rmSync(root, { recursive: true, force: true }); }
}

for (const target of targets) {
  test(`real installer retains exact legal payload and aliases: ${target[2]}`, () => fixture(target));
  test(`real installer updates valid existing legal payload: ${target[2]}`, () => fixture(target, undefined, true));
}
for (const name of legalFiles) {
  for (const kind of ['missing', 'empty', 'directory', 'symlink']) {
    for (const existing of [false, true]) {
      test(`real installer refuses ${kind} ${name}, existing=${existing}`, () => fixture(targets[0], { name, kind }, existing));
    }
  }
}
test('real installer checks the downloaded archive digest before installation', () => fixture(targets[0], undefined, false, true));
for (const name of legalFiles) {
  for (const kind of ['symlink', 'directory']) {
    test(`real installer preflights existing ${kind} destination ${name}`, () => fixture(targets[0], undefined, true, false, { name, kind }));
  }
}
