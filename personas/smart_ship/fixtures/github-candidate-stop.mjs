import {spawnSync} from 'node:child_process';
import {existsSync, readFileSync, writeFileSync} from 'node:fs';
import {watch} from 'node:fs/promises';
import {dirname} from 'node:path';

const [preparedStop, accepted, remote, program, ...args] = process.argv.slice(2);
if (!existsSync(preparedStop)) {
  const events = watch(dirname(preparedStop), {signal: AbortSignal.timeout(5000)});
  try {
    if (!existsSync(preparedStop)) {
      for await (const _event of events) {
        if (existsSync(preparedStop)) break;
      }
    }
  } finally {
    await events.return();
  }
}
const stop = JSON.parse(readFileSync(preparedStop, 'utf8'));
if (!/^refs\/heads\/[A-Za-z0-9/-]+$/.test(stop.ref) ||
    !/^[a-f0-9]{40}$/.test(stop.commit) || !/^[a-f0-9]{40}$/.test(stop.expected))
  throw new Error('invalid controller-prepared stop');
for (const command of [
  ['fetch', '--quiet', '--no-write-fetch-head', '--', stop.workspace, stop.commit],
  ['update-ref', stop.ref, stop.commit, stop.expected],
]) {
  const result = spawnSync('git', command, {cwd: remote, encoding: 'utf8', timeout: 10000});
  if (result.status !== 0) throw new Error(result.stderr || 'stop CAS failed');
}
writeFileSync(accepted, 'stopped');
const prepared = spawnSync(program, args, {encoding: 'utf8', timeout: 10000});
process.stdout.write(prepared.stdout || '');
process.stderr.write(prepared.stderr || '');
process.exitCode = prepared.status ?? 1;
