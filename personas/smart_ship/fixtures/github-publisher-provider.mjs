import {spawnSync} from 'node:child_process';
import {writeFileSync} from 'node:fs';
import {dirname} from 'node:path';

// Test-owned provider. It consumes the actual SDK mutation, atomically applies its
// exact guards with stock Git, then returns the publisher's original remote read.
const [remote, body, mode, report, binary, stopScript, store, ...read] = process.argv.slice(2);
const operation = JSON.parse(body).variables.input;
const updates = operation.refUpdates;
const cleanup = updates.length === 1 &&
  /^refs\/heads\/smart-ship-objects\/[a-f0-9]{40}$/.test(updates[0].name) &&
  updates[0].afterOid === '0'.repeat(40);
if ((!cleanup && updates.length !== 4) || updates.some(row => row.force ||
    !/^refs\/heads\/[A-Za-z0-9/-]+$/.test(row.name) ||
    !/^[a-f0-9]{40}$/.test(row.beforeOid) || !/^[a-f0-9]{40}$/.test(row.afterOid))) {
  throw new Error('invalid fixture atomic request');
}
function checked(program, args, options = {}) {
  const result = spawnSync(program, args, {encoding: 'utf8', timeout: 20000, ...options});
  if (result.status !== 0) throw new Error(result.stderr || 'fixture child failed');
  return result;
}
if (!cleanup && mode === 'stop') {
  checked(binary, ['run', '--standalone', stopScript, '--', store], {
    cwd: dirname(store),
    env: {PATH: process.env.PATH, HOME: process.env.HOME,
      TOKIO_WORKER_THREADS: '2', RAYON_NUM_THREADS: '2'},
  });
} else if (!cleanup && mode === 'head_change') {
  const head = updates.find(row => row.name === 'refs/heads/feature/42');
  const destination = updates.find(row => row.name === 'refs/heads/main');
  checked('git', ['update-ref', head.name, destination.beforeOid, head.beforeOid], {cwd: remote});
}
const protocol = 'start\n' + updates.map(row =>
  `update ${row.name} ${row.afterOid} ${row.beforeOid}\n`).join('') + 'prepare\ncommit\n';
const applied = !cleanup && mode === 'unapplied' ? {status: 1} : spawnSync('git', ['update-ref', '--stdin'], {
  cwd: remote, input: protocol, encoding: 'utf8', timeout: 20000,
});
writeFileSync(report, JSON.stringify({applied: applied.status === 0, updates: updates.length}));
if (!cleanup && mode === 'lost_readback') {
  process.stderr.write('fixture first authority read unavailable\n');
  process.exit(1);
}
const observed = checked(read[0], read.slice(1));
process.stdout.write(observed.stdout);
process.stderr.write(observed.stderr);
