// Exercise ordered windows through the owning historical rehearsal reader.
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const [root, directory, source, producer, child] = process.argv.slice(2);
// The shell caller also tests timestamp normalization; retain that real input.
const baseline = readFileSync(join(directory, 'consumer'), 'utf8')
  .replace(/^\S+Z /gm, '');
const [, dispatchBody, terminalBody] = baseline.split('##[group]Run CANARY_REPOSITORY=');
const dispatch = '##[group]Run CANARY_REPOSITORY=' + dispatchBody;
const cleanupIndex = terminalBody.indexOf('Post job cleanup.');
const terminal = '##[group]Run CANARY_REPOSITORY=' + terminalBody.slice(0, cleanupIndex);
const cleanup = terminalBody.slice(cleanupIndex);
const pending = terminal.replace(
  `CONSUMER_CANARY verdict=fail conclusion=cancelled run=${child} wall_seconds=1510\n` +
  '##[error]Process completed with exit code 1.\n',
  `CONSUMER_CANARY pending run=${child} status=in_progress wall_seconds=2700\n`,
);
const launcher = join(directory, 'read-windows.sh');
writeFileSync(launcher,
  '#!/usr/bin/env bash\nset -euo pipefail\nsource "$1/scripts/lib/release_consumer_verdict.sh"\n' +
  'release_failed_rehearsal_observation "$2/resolver" "$2/consumer" "$2/authorization" "$3" "$4" "$5"\n');

function history(count) {
  const windows = Array.from({ length: count - 1 }, (_, index) =>
    pending.replace('wall_seconds=2700', `wall_seconds=${2700 * (index + 1)}`));
  windows.push(terminal.replace('wall_seconds=1510', `wall_seconds=${2700 * (count - 1) + 100}`));
  return [dispatch, windows];
}
function check(name, owner, windows, accepted = false) {
  writeFileSync(join(directory, 'consumer'), owner + windows.join('') + cleanup);
  const result = spawnSync('bash', [launcher, root, directory, source, producer, child], { encoding: 'utf8' });
  if (result.error || (result.status === 0) !== accepted) {
    throw new Error(`${name}: unexpected acceptance=${result.status === 0}: ${result.error ?? result.stderr}`);
  }
}
for (const count of [1, 2, 3]) check(`valid ${count} windows`, ...history(count), true);
for (const count of [1, 2, 3]) {
  const [dispatch, observations] = history(count);
  const last = observations.length - 1;
  for (const [name, record] of [
    ['plain runner record', 'Unrelated runner record'],
    ['canary record', `CONSUMER_CANARY dispatched run=${child} ref=default started_at=1700000000`],
    ['second process exit', '##[error]Process completed with exit code 1.'],
  ]) {
    const changed = [...observations];
    changed[last] = changed[last].replace('##[error]Process completed with exit code 1.\n',
      `##[error]Process completed with exit code 1.\n${record}\n`);
    check(`trailing ${name} across ${count} windows`, dispatch, changed);
  }
  const separated = [...observations];
  separated[last] = separated[last].replace('##[error]Process completed with exit code 1.',
    'Unrelated record between verdict and exit\n##[error]Process completed with exit code 1.');
  check(`nonadjacent terminal verdict and exit across ${count} windows`, dispatch, separated);
  const reversed = [...observations];
  reversed[last] = reversed[last].replace(/(CONSUMER_CANARY verdict=[^\n]+)\n(##\[error\]Process completed with exit code 1\.)/,
    '$2\n$1');
  check(`reversed terminal verdict and exit across ${count} windows`, dispatch, reversed);
}
const [owner, windows] = history(3);
check('observer before dispatch', windows[0] + owner, windows.slice(1));
check('four windows', ...history(4));
check('pending after terminal', owner, [windows[0], windows[2], windows[1]]);
check('all observer clocks rewritten', owner,
  windows.map(w => w.replace('CANARY_STARTED_AT: 1700000000', 'CANARY_STARTED_AT: 1700000001')));
check('owner clock rewritten', owner.replace('started_at=1700000000', 'started_at=1700000001'), windows);
check('owner clock absent', owner.replace(' started_at=1700000000', ''), windows);
for (const field of ['CANARY_STARTED_AT', 'CANARY_WINDOW_SECONDS', 'CANARY_DEADLINE_SECONDS']) {
  check(`missing ${field}`, owner, windows.map(w =>
    w.split('\n').filter(line => !line.includes(field)).join('\n')));
}
for (const [field, oldValue, newValue] of [
  ['CANARY_STARTED_AT', '1700000000', '1700000001'],
  ['CANARY_WINDOW_SECONDS', '2700', '2701'],
  ['CANARY_DEADLINE_SECONDS', '7200', '7201'],
  ['CANARY_RUN_ID', child, '1'],
]) {
  check(`changed ${field}`, owner, [windows[0], windows[1].replace(`${field}: ${oldValue}`, `${field}: ${newValue}`), windows[2]]);
}
check('bare pending', owner, [windows[0].replace('status=in_progress wall_seconds=2700', ''), ...windows.slice(1)]);
check('premature renewal', owner, [windows[0].replace('wall_seconds=2700', 'wall_seconds=4000'), ...windows.slice(1)]);
check('decreasing elapsed', owner, [...windows.slice(0, 2), windows[2].replace('wall_seconds=5500', 'wall_seconds=5300')]);
check('deadline exhausted', owner, [...windows.slice(0, 2), windows[2].replace('wall_seconds=5500', 'wall_seconds=7200')]);
check('terminal error in another step', owner,
  [...windows.slice(0, 2), windows[2].replace('##[error]', '##[group]Run echo unrelated\n##[error]')]);
check('duplicate pending', owner, [windows[0].replace(`CONSUMER_CANARY pending run=${child}`,
  `CONSUMER_CANARY pending run=${child} status=in_progress wall_seconds=2700\nCONSUMER_CANARY pending run=${child}`), ...windows.slice(1)]);
console.log('Historical rehearsal windows: 1/2/3 accepted; 34 clock, ordering, budget, and terminal false proofs refused.');
