import {spawnSync} from 'node:child_process';
import {readFileSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';

// Test-owned provider. It consumes the actual SDK mutation, atomically applies its
// exact guards with stock Git, then returns the publisher's original remote read.
const [remote, body, mode, report, binary, stopScript, store, ...read] = process.argv.slice(2);
const operation = JSON.parse(body).variables.input;
const updates = operation.refUpdates;
const deletions = updates.filter(row =>
  /^refs\/heads\/smart-ship-(objects|candidates)\/[a-f0-9]{40}$/.test(row.name) &&
  row.afterOid === '0'.repeat(40));
const authorityRef = JSON.parse(readFileSync(store, 'utf8')).ref;
const cleanup = deletions.length === 1 && (updates.length === 1 ||
  ([2, 3].includes(updates.length) && updates.some(row => row.name === authorityRef &&
    row.beforeOid === row.afterOid)));
const repair = mode === 'repair' && updates.length === 2 &&
  updates.some(row => row.name === 'refs/heads/main') &&
  updates.some(row => row.name === authorityRef);
if ((!cleanup && !repair && updates.length !== 4) || updates.some(row => row.force ||
    !/^refs\/heads\/[A-Za-z0-9/-]+$/.test(row.name) ||
    !/^[a-f0-9]{40}$/.test(row.beforeOid) || !/^[a-f0-9]{40}$/.test(row.afterOid))) {
  throw new Error('invalid fixture atomic request');
}
function checked(program, args, options = {}) {
  const result = spawnSync(program, args, {encoding: 'utf8', timeout: 20000, ...options});
  if (result.status !== 0) throw new Error(JSON.stringify({program, status: result.status,
    signal: result.signal, error: result.error?.code, stderr: result.stderr}));
  return result;
}
if (cleanup && ['cleanup_stop', 'candidate_cleanup:stop'].includes(mode)) {
  // The stop is prepared by the actual controller owner, then accepted here by exact Git CAS.
  // This preserves the race without compiling another Harn graph inside the timed provider read.
  const stop = JSON.parse(readFileSync(join(dirname(store), 'cleanup-stop-write.json'), 'utf8'));
  if (stop.ref !== authorityRef || !/^[a-f0-9]{40}$/.test(stop.commit) ||
      !/^[a-f0-9]{40}$/.test(stop.expected)) throw new Error('invalid prepared stop');
  checked('git', ['fetch', '--quiet', '--no-write-fetch-head', '--', stop.workspace, stop.commit], {cwd: remote});
  checked('git', ['update-ref', stop.ref, stop.commit, stop.expected], {cwd: remote});
} else if (!cleanup && mode === 'stop') {
  checked(binary, ['run', '--standalone', stopScript, '--', store], {
    cwd: dirname(store),
    env: {PATH: process.env.PATH, HOME: process.env.HOME,
      TOKIO_WORKER_THREADS: '2', RAYON_NUM_THREADS: '2'},
  });
} else if (cleanup && mode === 'candidate_cleanup:destination_race') {
  // Inject the destination change independently of whether the caller supplied its guard.
  const before = checked('git', ['rev-parse', 'refs/heads/main'], {cwd: remote}).stdout.trim();
  const parent = checked('git', ['rev-parse', `${before}^`], {cwd: remote}).stdout.trim();
  checked('git', ['update-ref', 'refs/heads/main', parent, before], {cwd: remote});
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
