#!/usr/bin/env node
// Host observation only. Never changes the compiler command, flags or wrapper.
const { execFile } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { performance } = require('node:perf_hooks');

class ObservationRefusal extends Error {
  constructor(stage, cause, message) {
    super(message);
    this.stage = stage;
    this.cause = cause;
  }
}

// Publish closed diagnostic facts, never the host error's paths or output.
function hostCommand(program, args, maxBuffer, stage, timeoutMs = 3000) {
  let child;
  const result = new Promise((resolve, reject) => {
    child = execFile(program, args, { encoding: 'utf8', timeout: timeoutMs, killSignal: 'SIGKILL', maxBuffer }, (error, stdout) => {
      if (error) {
        const cause = error.code === 'ENOENT' ? 'unavailable'
          : error.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER' ? 'clipped'
          : error.killed && error.signal === 'SIGKILL' ? 'timed_out'
          : 'command_failed';
        reject(new ObservationRefusal(stage, cause, 'unmeasured host observation'));
      } else if (stdout.length === 0) {
        reject(new ObservationRefusal(stage, 'empty_output', 'unmeasured host observation'));
      } else resolve(stdout);
    });
  });
  return { child, result };
}

function census(text, root, expectedRoot) {
  const rows = text.trim().split('\n').map(line => {
    const match = line.match(/^\s*(\d+)\s+(\d+)\s+(.{24})\s+(.+)$/);
    if (!match) throw new ObservationRefusal('process_census', 'malformed_row', 'unmeasured process census');
    const row = { pid: Number(match[1]), parent: Number(match[2]), start: match[3], executable: path.basename(match[4]) };
    if (!Number.isSafeInteger(row.pid) || row.pid <= 0 || !Number.isSafeInteger(row.parent)
      || row.parent < 0 || !Number.isFinite(Date.parse(row.start)) || !row.executable) throw new ObservationRefusal('process_census', 'invalid_identity', 'unmeasured process identity');
    return row;
  });
  const byPid = new Map(rows.map(row => [row.pid, row]));
  if (byPid.size !== rows.length || !byPid.has(root)) throw new ObservationRefusal('process_census', 'incomplete', 'incomplete process census');
  const owner = byPid.get(root);
  if (expectedRoot && !sameProcess(expectedRoot, owner)) throw new ObservationRefusal('process_census', 'owner_changed', 'changed observation owner');
  const compilers = rows.filter(row => {
    if (!['rustc', 'clippy-driver'].includes(row.executable)) return false;
    const seen = new Set([row.pid]);
    let parent = row.parent;
    while (parent !== root) {
      if (parent <= 1) return false;
      if (seen.has(parent) || !byPid.has(parent)) throw new ObservationRefusal('process_census', 'incomplete_ancestry', 'unmeasured process ancestry');
      seen.add(parent);
      parent = byPid.get(parent).parent;
    }
    return true;
  });
  return { owner, compilers };
}

function symbolStacks(text, target) {
  const processHeader = text.match(/^Process:\s+([^\n]+?)\s+\[(\d+)\]\s*$/m);
  if (!processHeader || Number(processHeader[2]) !== target.pid
    || processHeader[1].trim() !== target.executable) throw new ObservationRefusal('sample_parse', 'identity_mismatch', 'unmeasured sample identity');
  // Do not publish sample's host headers, binary paths, argv or environment.
  const begin = text.indexOf('Call graph:\n');
  const end = text.indexOf('\nTotal number in stack', begin);
  if (begin < 0 || end < 0) throw new ObservationRefusal('sample_parse', 'incomplete_graph', 'unmeasured symbol sample');
  const graph = text.slice(begin, end);
  if (graph.length > 131072) throw new ObservationRefusal('sample_parse', 'oversized_graph', 'unmeasured compiler symbols');
  if (!/rustc[_:]/.test(graph)) throw new ObservationRefusal('sample_parse', 'missing_compiler_symbols', 'unmeasured compiler symbols');
  return graph;
}

function sameProcess(left, right) {
  return right && left.pid === right.pid && left.parent === right.parent
    && left.start === right.start && left.executable === right.executable;
}

async function observe(root, output, run, attempt, sourceCommit, sourceTree) {
  const started = performance.now();
  const receipt = { schema: 'harn.macos_compiler_samples.v1', run, attempt, sourceCommit, sourceTree,
    beganAt: new Date().toISOString(), status: 'UNMEASURED', maximumSamples: 4,
    requiredPersistenceMs: 60000, observedCompilerIdentities: 0, samples: [],
    refusal: { stage: 'selection', cause: 'no_persistent_owned_compiler' } };
  let stopped = false;
  let child;
  let owner;
  process.on('SIGTERM', () => { stopped = true; child?.kill('SIGTERM'); });
  const command = async (program, args, maxBuffer, stage) => {
    const pending = hostCommand(program, args, maxBuffer, stage);
    child = pending.child;
    try { return await pending.result; }
    finally { child = undefined; }
  };
  const read = async () => {
    const value = census(await command('/bin/ps', ['-axo', 'pid=,ppid=,lstart=,comm='], 1048576, 'process_command'), root, owner);
    owner = value.owner;
    receipt.owner = owner;
    return value.compilers;
  };
  try {
    if (process.platform !== 'darwin' || !Number.isSafeInteger(root) || root <= 1
      || !/^[1-9][0-9]*$/.test(run) || !/^[1-9][0-9]*$/.test(attempt)
      || !/^[0-9a-f]{40}$/.test(sourceCommit) || !/^[0-9a-f]{40}$/.test(sourceTree)) throw new ObservationRefusal('execution_identity', 'unsupported_or_invalid', 'unmeasured execution identity');
    const sampled = new Set();
    const firstSeen = new Map();
    while (!stopped && performance.now() - started < 3600000 && receipt.samples.length < 4) {
      const processes = await read();
      for (const target of processes) {
        const identity = `${target.pid}:${target.start}`;
        if (!firstSeen.has(identity)) firstSeen.set(identity, performance.now());
        if (firstSeen.size > 8192) throw new ObservationRefusal('selection', 'population_limit', 'unmeasured compiler population');
        receipt.observedCompilerIdentities = firstSeen.size;
        // Observe long-lived compiler units rather than exhausting the bounded
        // sample allowance on short setup/dependency checks.
        if (performance.now() - firstSeen.get(identity) < 60000) continue;
        if (sampled.has(identity) || stopped) continue;
        sampled.add(identity);
        const before = performance.now() - started;
        const beganAt = new Date().toISOString();
        const raw = await command('/usr/bin/sample', [String(target.pid), '2', '10'], 262144, 'sample_command');
        const stacks = symbolStacks(raw, target);
        if (stopped || !sameProcess(target, (await read()).find(row => row.pid === target.pid))) {
          receipt.refusal = { stage: 'sample_identity', cause: 'changed_or_interrupted' };
          continue;
        }
        const index = receipt.samples.length;
        fs.writeFileSync(path.join(output, `compiler-symbols-${index}.txt`), stacks, { flag: 'wx' });
        receipt.samples.push({ ...target, beganAt, endedAt: new Date().toISOString(),
          beganMs: before, endedMs: performance.now() - started,
          symbols: `compiler-symbols-${index}.txt` });
        receipt.status = 'MEASURED';
        delete receipt.refusal;
        if (receipt.samples.length === 4) break;
      }
      if (!stopped && receipt.samples.length < 4) await new Promise(resolve => setTimeout(resolve, 1000));
    }
  } catch (error) {
    // Diagnostic absence never changes the strict compiler's outcome.
    receipt.status = 'UNMEASURED';
    receipt.reason = 'host observation unavailable, incomplete or clipped';
    receipt.refusal = error instanceof ObservationRefusal
      ? { stage: error.stage, cause: error.cause }
      : { stage: 'observation', cause: 'unexpected_failure' };
  } finally {
    if (receipt.status === 'UNMEASURED' && stopped) receipt.refusal = { stage: 'settlement', cause: 'interrupted' };
    receipt.endedMs = performance.now() - started;
    receipt.endedAt = new Date().toISOString();
    fs.writeFileSync(path.join(output, 'compiler-sample.json'), JSON.stringify(receipt, null, 2), { flag: 'wx' });
  }
}

module.exports = { census, sameProcess, symbolStacks, hostCommand };
if (require.main === module) {
  observe(Number(process.argv[2]), process.argv[3], process.argv[4] ?? '', process.argv[5] ?? '', process.argv[6] ?? '', process.argv[7] ?? '')
    .catch(() => { process.exitCode = 1; });
}
