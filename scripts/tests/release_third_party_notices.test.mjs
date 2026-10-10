import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, mkdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { render, configuration, normalizeInventory, sourceFingerprint, verify } from '../release_third_party_notices.mjs';

test('modified workspace components retain their actual attribution notice', () => {
  const pkg = { name: 'harn-aws-config', version: '0.10.159', source: null,
    notices: [{ name: 'NOTICE.harn', text: 'Modified AWS SDK source; retain upstream attribution.' }] };
  const inventory = { crates: [{ package: pkg, license: 'Apache-2.0' }],
    licenses: [{ id: 'Apache-2.0', text: 'Complete Apache license text.', used_by: [{ crate: pkg }] }] };
  const text = render(inventory);
  assert.match(text, /Modified AWS SDK source; retain upstream attribution/);
  assert.match(text, /NOTICE\.harn/);
  assert.match(text, /Complete Apache license text/);
  assert.doesNotMatch(text, /api\/v1\/crates\/harn-aws-config/, 'workspace source is not claimed to be published');
  const withPackage = packageValue => ({ ...inventory, crates: [{ package: packageValue, license: 'Apache-2.0' }] });
  assert.doesNotMatch(render(withPackage({ ...pkg, notices: [] })), /Source: packaged workspace component/);
  assert.throws(() => render(withPackage({ ...pkg, source: 'git+https://example.invalid/component' })), /Unsupported dependency source/);
  assert.throws(() => render(withPackage({ ...pkg, notices: [{ name: 'NOTICE.harn', text: '' }] })), /NOTICE/);
  assert.throws(() => render({ ...inventory, crates: [{ package: { ...pkg, notices: undefined }, license: 'Apache-2.0' }] }), /NOTICE inventory/);
});

test('release inventory reaches full texts, NOTICE and exact source availability', () => {
  const dir = mkdtempSync(join(tmpdir(), 'harn-notices-test-'));
  try {
    writeFileSync(join(dir, 'NOTICE'), 'Retain this copyright and attribution.');
    mkdirSync(join(dir, 'vendor'));
    writeFileSync(join(dir, 'vendor/NOTICES.md'), 'Nested attribution must survive.');
    const pkg = { name: 'component', version: '1.2.3', manifest_path: join(dir, 'Cargo.toml'),
      source: 'registry+https://github.com/rust-lang/crates.io-index' };
    const inventory = normalizeInventory({ crates: [{ package: pkg, license: 'MPL-2.0' }],
      licenses: [{ id: 'MPL-2.0', text: 'Full component license text.', used_by: [{ crate: pkg }] }] });
    const text = render(inventory);
    assert.doesNotMatch(JSON.stringify(inventory), /manifest_path/);
    assert.ok(!JSON.stringify(inventory).includes(dir), 'retained material contains no host path');
    assert.match(text, /Full component license text/);
    assert.match(text, /Retain this copyright and attribution/);
    assert.match(text, /Nested attribution must survive/);
    assert.match(text, /api\/v1\/crates\/component\/1.2.3\/download/);
    assert.match(text, /MPL-2.0 source availability/);
    assert.throws(() => render({ crates: [], licenses: [] }), /Empty/);
    assert.throws(() => render({ ...inventory, licenses: [{ ...inventory.licenses[0], text: '' }] }), /full/);
    const other = { ...pkg, name: 'uncovered' };
    assert.throws(() => render({ ...inventory, crates: [...inventory.crates, { package: other, license: 'MIT' }] }), /without full/);
    assert.throws(() => render({ ...inventory, licenses: [{ ...inventory.licenses[0], used_by: [{ crate: other }] }] }), /unknown crate/);
    writeFileSync(join(dir, 'NOTICE'), '');
    assert.throws(() => normalizeInventory({ crates: [{ package: pkg, license: 'MPL-2.0' }],
      licenses: inventory.licenses }), /NOTICE/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('offline packaged verification refuses missing material and binds bytes to current source', () => {
  const dir = mkdtempSync(join(tmpdir(), 'harn-offline-notice-test-'));
  try {
    for (const path of ['.github', 'scripts', 'out']) mkdirSync(join(dir, path));
    writeFileSync(join(dir, 'Cargo.toml'), '[workspace]\nmembers = ["component"]\n');
    mkdirSync(join(dir, 'component'));
    writeFileSync(join(dir, 'component/Cargo.toml'), '[package]\nname = "component"\n');
    for (const file of ['Cargo.lock', 'deny.toml', '.github/release-runner-policy.json',
      'package.json', 'package-lock.json', 'scripts/release_third_party_notices.mjs', 'LICENSE-MIT', 'LICENSE-APACHE']) {
      writeFileSync(join(dir, file), `nonempty ${file}`);
    }
    const pkg = { name: 'component', version: '1.2.3', source: 'registry+https://github.com/rust-lang/crates.io-index',
      notices: [{ name: 'NOTICE', text: 'Retained attribution' }] };
    const about = { crates: [{ package: pkg, license: 'MIT' }],
      licenses: [{ id: 'MIT', text: 'Full license text', used_by: [{ crate: pkg }] }] };
    const inventory = { schemaVersion: 1, sourceFingerprint: sourceFingerprint(dir), about };
    const writeInventory = value => writeFileSync(join(dir, 'out/release-license-inventory.json'), JSON.stringify(value));
    const writeText = value => writeFileSync(join(dir, 'out/THIRD-PARTY-NOTICES.txt'), value);
    for (const file of ['LICENSE-MIT', 'LICENSE-APACHE']) writeFileSync(join(dir, 'out', file), readFileSync(join(dir, file)));
    writeInventory(inventory); writeText(render(about));
    verify(dir, join(dir, 'out'));
    writeText('tampered nonempty license material');
    assert.throws(() => verify(dir, join(dir, 'out')), /material differs/);
    writeText('');
    assert.throws(() => verify(dir, join(dir, 'out')), /material differs/);
    writeText(render(about));
    writeInventory({ ...inventory, about: { ...about, licenses: [{ ...about.licenses[0], text: '' }] } });
    assert.throws(() => verify(dir, join(dir, 'out')), /full MIT license/);
    writeInventory(inventory);
    rmSync(join(dir, 'out/release-license-inventory.json'));
    assert.throws(() => verify(dir, join(dir, 'out')), /ENOENT/);
    writeInventory(inventory);
    writeFileSync(join(dir, 'Cargo.lock'), 'different dependency inputs');
    assert.throws(() => verify(dir, join(dir, 'out')), /differs from current source/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

for (const change of ['add', 'edit', 'remove']) {
  test(`retained workspace NOTICE inventory rejects a later ${change}`, () => {
    const dir = mkdtempSync(join(tmpdir(), 'harn-workspace-notice-'));
    try {
      for (const path of ['.github', 'scripts', 'out', 'component']) mkdirSync(join(dir, path));
      writeFileSync(join(dir, 'Cargo.toml'), '[workspace]\nmembers = ["component"]\n');
      writeFileSync(join(dir, 'component/Cargo.toml'), '[package]\nname = "component"\n');
      for (const file of ['Cargo.lock', 'deny.toml', '.github/release-runner-policy.json',
        'package.json', 'package-lock.json', 'scripts/release_third_party_notices.mjs', 'LICENSE-MIT', 'LICENSE-APACHE']) {
        writeFileSync(join(dir, file), `nonempty ${file}`);
      }
      const notice = join(dir, 'component/NOTICE.harn');
      if (change !== 'add') writeFileSync(notice, 'Original workspace attribution.');
      const pkg = { name: 'component', version: '1.2.3', source: null,
        notices: change === 'add' ? [] : [{ name: 'NOTICE.harn', text: readFileSync(notice, 'utf8') }] };
      const about = { crates: [{ package: pkg, license: 'Apache-2.0' }],
        licenses: [{ id: 'Apache-2.0', text: 'Full Apache license text', used_by: [{ crate: pkg }] }] };
      writeFileSync(join(dir, 'out/release-license-inventory.json'), JSON.stringify({
        schemaVersion: 1, sourceFingerprint: sourceFingerprint(dir), about,
      }));
      writeFileSync(join(dir, 'out/THIRD-PARTY-NOTICES.txt'), render(about));
      for (const file of ['LICENSE-MIT', 'LICENSE-APACHE']) writeFileSync(join(dir, 'out', file), readFileSync(join(dir, file)));
      verify(dir, join(dir, 'out'));
      if (change === 'remove') rmSync(notice);
      else writeFileSync(notice, 'Changed workspace attribution.');
      assert.throws(() => verify(dir, join(dir, 'out')), /differs from current source/);
    } finally { rmSync(dir, { recursive: true, force: true }); }
  });
}

test('license and platform policy are read from their owners', () => {
  const config = configuration(new URL('../..', import.meta.url).pathname);
  assert.match(config, /accepted = \["Apache-2.0"/);
  assert.match(config, /x86_64-pc-windows-msvc/);
  assert.match(config, /aarch64-apple-darwin/);
});

test('TOML comments cannot admit licenses and alternate valid layouts preserve policy', () => {
  const dir = mkdtempSync(join(tmpdir(), 'harn-license-policy-test-'));
  try {
    mkdirSync(join(dir, '.github'));
    writeFileSync(join(dir, '.github/release-runner-policy.json'), JSON.stringify({ targets: [{ target: 'release-target' }] }));
    writeFileSync(join(dir, 'deny.toml'), '# allow = ["Forbidden"]\n[licenses]\nallow = [\n"MIT", # "Forbidden"\n]\n');
    const config = configuration(dir);
    assert.match(config, /accepted = \["MIT"\]/);
    assert.doesNotMatch(config, /Forbidden/);
    writeFileSync(join(dir, 'deny.toml'), 'licenses = { allow = ["MIT"] }\n');
    assert.equal(configuration(dir), config);
    writeFileSync(join(dir, 'deny.toml'), '["licenses"]\n"allow" = [\'MIT\']\n');
    assert.equal(configuration(dir), config);
    writeFileSync(join(dir, 'deny.toml'), '[licenses]\n# allow = ["MIT"]\n');
    assert.throws(() => configuration(dir), /Empty or duplicate/);
    writeFileSync(join(dir, 'deny.toml'), '[licenses]\nallow = "MIT"\n');
    assert.throws(() => configuration(dir), /Empty or duplicate/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('actual Unix packaging includes exact notices and refuses missing or empty material', () => {
  const workflow = readFileSync(new URL('../../.github/workflows/build-release-binaries.yml', import.meta.url), 'utf8');
  const step = workflow.split('      - name: Package (unix)\n')[1]?.split('\n      - name: ')[0];
  const script = step?.split('        run: |\n')[1]?.split('\n').map(line => line.slice(10)).join('\n');
  assert.ok(script?.includes('tar czf'), 'owning packaging step must be reached');
  for (const target of ['aarch64-apple-darwin', 'x86_64-unknown-linux-gnu']) {
    const dir = mkdtempSync(join(tmpdir(), 'harn-archive-test-'));
    try {
      mkdirSync(join(dir, `target/${target}/release`), { recursive: true });
      writeFileSync(join(dir, `target/${target}/release/harn`), 'binary fixture');
      writeFileSync(join(dir, `target/${target}/release/harn-container-probe`), 'probe fixture');
      mkdirSync(join(dir, 'dist/release-notices'), { recursive: true });
      const files = ['LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt'];
      for (const file of files) writeFileSync(join(dir, 'dist/release-notices', file), `${file} full fixture text\n`);
      const run = () => spawnSync('bash', ['-c', script], { cwd: dir, encoding: 'utf8',
        env: { ...process.env, HARN_RELEASE_TARGET: target, GITHUB_STEP_SUMMARY: join(dir, 'summary') } });
      const result = run();
      assert.equal(result.status, 0, result.stderr);
      const archive = join(dir, `dist/harn-${target}.tar.gz`);
      for (const file of files) {
        const member = spawnSync('tar', ['-xOzf', archive, file], { encoding: 'utf8' });
        assert.equal(member.status, 0, member.stderr);
        assert.equal(member.stdout, `${file} full fixture text\n`);
      }
      // Remove previous package outputs so both failures exercise fresh assembly.
      for (const file of ['harn', 'harn-lsp', 'harn-dap']) rmSync(join(dir, 'dist', file));
      writeFileSync(join(dir, 'dist/release-notices/THIRD-PARTY-NOTICES.txt'), '');
      assert.notEqual(run().status, 0, 'empty notices must refuse packaging');
      rmSync(join(dir, 'dist/release-notices/THIRD-PARTY-NOTICES.txt'));
      assert.notEqual(run().status, 0, 'absent notices must refuse packaging');
    } finally { rmSync(dir, { recursive: true, force: true }); }
  }
});
