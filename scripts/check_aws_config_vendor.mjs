import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const repository = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ownerFiles = new Set(['upstream.json', 'upstream.patch', 'README.harn.md', 'NOTICE']);
const digest = bytes => createHash('sha256').update(bytes).digest('hex');

function requireRegular(path) {
  if (!lstatSync(path).isFile()) throw new Error(`Expected a regular file: ${path}`);
}

function tree(directory, prefix = '') {
  const files = new Map();
  for (const name of readdirSync(join(directory, prefix)).sort()) {
    const relative = prefix ? `${prefix}/${name}` : name;
    const path = join(directory, relative);
    const stat = lstatSync(path);
    if (stat.isDirectory()) {
      for (const [file, hash] of tree(directory, relative)) files.set(file, hash);
    } else {
      requireRegular(path);
      files.set(relative, digest(readFileSync(path)));
    }
  }
  return files;
}

function command(executable, args, options = {}) {
  const result = spawnSync(executable, args, { encoding: 'utf8', ...options });
  if (result.error || result.status !== 0) {
    throw new Error(`${executable} failed: ${result.error?.message ?? result.stderr}`);
  }
  return result.stdout;
}

function metadata(directory) {
  const manifest = JSON.parse(readFileSync(join(directory, 'upstream.json'), 'utf8'));
  const fields = ['schema', 'crate', 'version', 'archive_sha256', 'patch_sha256'];
  if (Object.keys(manifest).sort().join() !== fields.sort().join() ||
      manifest.schema !== 'harn.aws_config_upstream.v1' || manifest.crate !== 'aws-config' ||
      !/^\d+\.\d+\.\d+$/.test(manifest.version) ||
      !/^[a-f0-9]{64}$/.test(manifest.archive_sha256) ||
      !/^[a-f0-9]{64}$/.test(manifest.patch_sha256)) {
    throw new Error('Invalid captured AWS SDK upstream identity');
  }
  requireRegular(join(directory, 'upstream.patch'));
  if (digest(readFileSync(join(directory, 'upstream.patch'))) !== manifest.patch_sha256) {
    throw new Error('Recorded AWS SDK patch checksum mismatch');
  }
  return manifest;
}

async function verifiedArchive(manifest, suppliedArchive) {
  const cache = join(process.env.XDG_CACHE_HOME || join(homedir(), '.cache'), 'harn', 'upstream-crates');
  const path = suppliedArchive ?? join(cache, `${manifest.crate}-${manifest.version}-${manifest.archive_sha256}.crate`);
  if (!existsSync(path)) {
    if (suppliedArchive) throw new Error(`Missing pristine AWS SDK archive: ${path}`);
    const url = `https://static.crates.io/crates/aws-config/aws-config-${manifest.version}.crate`;
    const response = await fetch(url);
    if (!response.ok) throw new Error(`Cannot fetch pristine AWS SDK archive: HTTP ${response.status}`);
    const bytes = Buffer.from(await response.arrayBuffer());
    if (digest(bytes) !== manifest.archive_sha256) throw new Error('Downloaded AWS SDK archive checksum mismatch');
    mkdirSync(cache, { recursive: true });
    try {
      writeFileSync(path, bytes, { flag: 'wx' });
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
    }
  }
  requireRegular(path);
  if (digest(readFileSync(path)) !== manifest.archive_sha256) {
    throw new Error('Pristine AWS SDK archive checksum mismatch');
  }
  return path;
}

/** Compare against a checksum-verified pristine archive plus the recorded patch. */
export async function verifyAwsConfigMirror(root = repository, suppliedArchive) {
  const directory = join(root, 'crates', 'harn-aws-config');
  const manifest = metadata(directory);
  const archive = await verifiedArchive(manifest, suppliedArchive);
  const prefix = `${manifest.crate}-${manifest.version}`;
  const entries = command('tar', ['-tzf', archive]).trim().split('\n');
  if (!entries.length || entries.some(entry => !entry.startsWith(`${prefix}/`) ||
      entry.split('/').some(part => part === '..' || part === ''))) {
    throw new Error('Invalid pristine AWS SDK archive paths');
  }
  const temporary = mkdtempSync(join(tmpdir(), 'harn-aws-config-verify-'));
  try {
    command('tar', ['-xzf', archive, '-C', temporary]);
    const pristine = join(temporary, prefix);
    command('git', ['apply', '--whitespace=error', join(directory, 'upstream.patch')], { cwd: pristine });
    const expected = tree(pristine);
    const actual = tree(directory);
    for (const name of ownerFiles) {
      if (!actual.delete(name)) throw new Error(`Missing AWS SDK owner file: ${name}`);
    }
    const drift = [...new Set([...expected.keys(), ...actual.keys()])].sort()
      .filter(name => expected.get(name) !== actual.get(name));
    if (!expected.size || drift.length) throw new Error(`AWS SDK mirror drift: ${drift.join(', ')}`);
    return { upstream: `${manifest.crate} ${manifest.version}`, files: expected.size,
      archive_sha256: manifest.archive_sha256, patch_sha256: manifest.patch_sha256 };
  } finally {
    rmSync(temporary, { recursive: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  let archive;
  if (process.argv.length > 2) {
    if (process.argv.length !== 4 || process.argv[2] !== '--archive') {
      throw new Error('Usage: check_aws_config_vendor.mjs [--archive <verified-crate-file>]');
    }
    archive = resolve(process.argv[3]);
  }
  try {
    console.log(JSON.stringify(await verifyAwsConfigMirror(repository, archive)));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
