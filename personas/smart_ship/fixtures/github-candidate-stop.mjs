import {spawnSync} from 'node:child_process';
import {dirname} from 'node:path';

const [binary, stopScript, store, program, ...args] = process.argv.slice(2);
const stopped = spawnSync(binary, ['run', '--standalone', stopScript, '--', store], {
  cwd: dirname(store),
  encoding: 'utf8',
  timeout: 20000,
  env: {PATH: process.env.PATH, HOME: process.env.HOME,
    TOKIO_WORKER_THREADS: '2', RAYON_NUM_THREADS: '2'},
});
if (stopped.status !== 0) {
  throw new Error(stopped.stderr || 'candidate stop did not complete');
}
const prepared = spawnSync(program, args, {encoding: 'utf8', timeout: 10000});
process.stdout.write(prepared.stdout || '');
process.stderr.write(prepared.stderr || '');
process.exitCode = prepared.status ?? 1;
