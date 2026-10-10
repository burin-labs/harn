#!/usr/bin/env node
// Host observation only. Never changes the compiler command, flags or wrapper.
const { execFile } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { performance } = require('node:perf_hooks');

function census(text, root, expectedRoot) {
  const rows = text.trim().split('\n').map(line => {
    const match = line.match(/^\s*(\d+)\s+(\d+)\s+(.{24})\s+(.+)$/);
    if (!match) throw new Error('unmeasured process census');
    const row = { pid: Number(match[1]), parent: Number(match[2]), start: match[3], executable: path.basename(match[4]) };
    if (!Number.isSafeInteger(row.pid) || row.pid <= 0 || !Number.isSafeInteger(row.parent)
      || row.parent < 0 || !Number.isFinite(Date.parse(row.start)) || !row.executable) throw new Error('unmeasured process identity');
    return row;
  });
  const byPid = new Map(rows.map(row => [row.pid, row]));
  if (byPid.size !== rows.length || !byPid.has(root)) throw new Error('incomplete process census');
  const owner = byPid.get(root);
  if (expectedRoot && !sameProcess(expectedRoot, owner)) throw new Error('changed observation owner');
  const compilers = rows.filter(row => {
    if (!['rustc', 'clippy-driver'].includes(row.executable)) return false;
    const seen = new Set([row.pid]);
    let parent = row.parent;
    while (parent !== root) {
      if (parent <= 1) return false;
      if (seen.has(parent) || !byPid.has(parent)) throw new Error('unmeasured process ancestry');
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
    || processHeader[1].trim() !== target.executable) throw new Error('unmeasured sample identity');
  // Do not publish sample's host headers, binary paths, argv or environment.
  const begin = text.indexOf('Call graph:\n');
  const end = text.indexOf('\nTotal number in stack', begin);
  if (begin < 0 || end < 0) throw new Error('unmeasured symbol sample');
  const graph = text.slice(begin, end);
  if (!/rustc[_:]/.test(graph) || graph.length > 131072) throw new Error('unmeasured compiler symbols');
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
    requiredPersistenceMs: 60000, observedCompilerIdentities: 0, samples: [] };
  let stopped = false;
  let child;
  let owner;
  process.on('SIGTERM', () => { stopped = true; child?.kill('SIGTERM'); });
  const command = (program, args, maxBuffer) => new Promise((resolve, reject) => {
    child = execFile(program, args, { encoding: 'utf8', timeout: 3000, killSignal: 'SIGKILL', maxBuffer }, (error, stdout) => {
      child = undefined;
      if (error) reject(new Error('unmeasured host observation'));
      else resolve(stdout);
    });
  });
  const read = async () => {
    const value = census(await command('/bin/ps', ['-axo', 'pid=,ppid=,lstart=,comm='], 1048576), root, owner);
    owner = value.owner;
    receipt.owner = owner;
    return value.compilers;
  };
  try {
    if (process.platform !== 'darwin' || !Number.isSafeInteger(root) || root <= 1
      || !/^[1-9][0-9]*$/.test(run) || !/^[1-9][0-9]*$/.test(attempt)
      || !/^[0-9a-f]{40}$/.test(sourceCommit) || !/^[0-9a-f]{40}$/.test(sourceTree)) throw new Error('unmeasured execution identity');
    const sampled = new Set();
    const firstSeen = new Map();
    while (!stopped && performance.now() - started < 3600000 && receipt.samples.length < 4) {
      const processes = await read();
      for (const target of processes) {
        const identity = `${target.pid}:${target.start}`;
        if (!firstSeen.has(identity)) firstSeen.set(identity, performance.now());
        if (firstSeen.size > 8192) throw new Error('unmeasured compiler population');
        receipt.observedCompilerIdentities = firstSeen.size;
        // Observe long-lived compiler units rather than exhausting the bounded
        // sample allowance on short setup/dependency checks.
        if (performance.now() - firstSeen.get(identity) < 60000) continue;
        if (sampled.has(identity) || stopped) continue;
        sampled.add(identity);
        const before = performance.now() - started;
        const beganAt = new Date().toISOString();
        const raw = await command('/usr/bin/sample', [String(target.pid), '2', '10'], 262144);
        const stacks = symbolStacks(raw, target);
        if (stopped || !sameProcess(target, (await read()).find(row => row.pid === target.pid))) continue;
        const index = receipt.samples.length;
        fs.writeFileSync(path.join(output, `compiler-symbols-${index}.txt`), stacks, { flag: 'wx' });
        receipt.samples.push({ ...target, beganAt, endedAt: new Date().toISOString(),
          beganMs: before, endedMs: performance.now() - started,
          symbols: `compiler-symbols-${index}.txt` });
        receipt.status = 'MEASURED';
        if (receipt.samples.length === 4) break;
      }
      if (!stopped && receipt.samples.length < 4) await new Promise(resolve => setTimeout(resolve, 1000));
    }
  } catch {
    // Diagnostic absence never changes the strict compiler's outcome.
    receipt.status = 'UNMEASURED';
    receipt.reason = 'host observation unavailable, incomplete or clipped';
  } finally {
    receipt.endedMs = performance.now() - started;
    receipt.endedAt = new Date().toISOString();
    fs.writeFileSync(path.join(output, 'compiler-sample.json'), JSON.stringify(receipt, null, 2), { flag: 'wx' });
  }
}

module.exports = { census, sameProcess, symbolStacks };
if (require.main === module) {
  observe(Number(process.argv[2]), process.argv[3], process.argv[4] ?? '', process.argv[5] ?? '', process.argv[6] ?? '', process.argv[7] ?? '')
    .catch(() => { process.exitCode = 1; });
}
