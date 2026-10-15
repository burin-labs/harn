import {spawnSync} from 'node:child_process';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

// This public, nonsecret fixture exercises the actual read-only parent secret store.
const payload = Buffer.from(JSON.stringify({version: 1, secrets: [{
  id: {namespace: 'fixture', name: 'github-token'},
  value: [...Buffer.from('synthetic-fixture-token')],
}]}));
const header = Buffer.alloc(4);
header.writeUInt32BE(payload.length);
const result = spawnSync(process.argv[2], [
  'run', '--standalone', '--parent-secret-stdin',
  join(dirname(fileURLToPath(import.meta.url)), 'github-effect-cases.harn'),
  '--', process.argv[3], process.argv[4],
], {
  input: Buffer.concat([header, payload]), encoding: 'utf8', timeout: 180000,
  env: {PATH: process.env.PATH, HOME: process.env.HOME},
});
process.stdout.write(result.stdout ?? '');
process.stderr.write(result.stderr ?? '');
process.exitCode = result.status === 0 ? 0 : 1;
