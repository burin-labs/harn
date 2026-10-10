import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { verifyAwsConfigMirror } from '../check_aws_config_vendor.mjs';

const repository = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

async function fixture(action) {
  const root = mkdtempSync(join(tmpdir(), 'harn-aws-config-control-'));
  try {
    mkdirSync(join(root, 'crates'));
    cpSync(join(repository, 'crates/harn-aws-config'), join(root, 'crates/harn-aws-config'), { recursive: true });
    await action(root, join(root, 'crates/harn-aws-config'));
  } finally {
    rmSync(root, { recursive: true });
  }
}

test('mirror reaches the complete verified pristine archive plus recorded patch', async () => {
  const result = await verifyAwsConfigMirror(repository);
  assert.equal(result.upstream, 'aws-config 1.12.0');
  assert.ok(result.files > 70, 'a nonempty complete source tree must be measured');
});

test('unrecorded SDK source and extra files fail the same complete comparison', async () => {
  await fixture(async (root, directory) => {
    const file = join(directory, 'src/provider_config.rs');
    const original = readFileSync(file);
    writeFileSync(file, Buffer.concat([original, Buffer.from('\n// unrecorded control\n')]));
    await assert.rejects(verifyAwsConfigMirror(root), /mirror drift: src\/provider_config.rs/);
    writeFileSync(file, original);
    writeFileSync(join(directory, 'unexpected.rs'), '// unrecorded control\n');
    await assert.rejects(verifyAwsConfigMirror(root), /mirror drift: unexpected.rs/);
  });
});

test('recorded patch and pristine archive identities cannot silently drift', async () => {
  await fixture(async (root, directory) => {
    const patch = join(directory, 'upstream.patch');
    const original = readFileSync(patch);
    writeFileSync(patch, Buffer.concat([original, Buffer.from('\n')]));
    await assert.rejects(verifyAwsConfigMirror(root), /patch checksum mismatch/);
    writeFileSync(patch, original);
    const archive = join(root, 'wrong.crate');
    writeFileSync(archive, 'not the reviewed archive');
    await assert.rejects(verifyAwsConfigMirror(root, archive), /archive checksum mismatch/);
  });
});

test('upstream lock provenance cannot become an active lock or change bytes', async () => {
  await fixture(async (root, directory) => {
    const provenance = join(directory, 'Cargo.lock.upstream');
    const original = readFileSync(provenance);
    const active = join(directory, 'Cargo.lock');
    writeFileSync(active, original);
    await assert.rejects(verifyAwsConfigMirror(root), /mirror drift: Cargo\.lock/);
    rmSync(active);
    writeFileSync(provenance, Buffer.concat([original, Buffer.from('\n')]));
    await assert.rejects(verifyAwsConfigMirror(root), /mirror drift: Cargo\.lock\.upstream/);
  });
});
