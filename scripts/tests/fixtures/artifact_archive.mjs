import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

/** Build a fresh one-member ZIP and measure the artifact actually served by a fixture. */
export function archiveFile(file, metadata) {
  const directory = fs.mkdtempSync(path.join(path.dirname(file), '.artifact-'));
  const archive = path.join(directory, 'artifact.zip');
  const zipped = spawnSync('zip', ['-j', archive, file], {
    env: {PATH: process.env.PATH, LC_ALL: 'C'}, encoding: 'utf8',
  });
  assert.equal(zipped.status, 0, zipped.stderr);
  const bytes = fs.readFileSync(archive);
  return {
    archive,
    metadata: {
      expired: false, ...metadata, size_in_bytes: bytes.length,
      digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
    },
  };
}

export function archiveJson(directory, filename, value, metadata) {
  const file = path.join(directory, filename);
  fs.writeFileSync(file, JSON.stringify(value));
  return archiveFile(file, metadata);
}
