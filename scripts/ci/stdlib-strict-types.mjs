import { execFileSync, spawnSync } from 'node:child_process';
import { resolve, relative, sep } from 'node:path';

const directory = 'crates/harn-stdlib/src/stdlib';
const frontier = new Set([
  `${directory}/agent/user.harn`,
  `${directory}/stdlib_agents.harn`,
]);
const requireReport = (condition, message) => {
  if (!condition) throw new Error(`checker measurement refused: ${message}`);
};
const count = value => Number.isSafeInteger(value) && value >= 0;

try {
  const execution = spawnSync(process.argv[2], ['check', '--json', '--strict-types', directory], {
    encoding: 'utf8', timeout: 300_000, maxBuffer: 16 * 1024 * 1024,
  });
  requireReport(!execution.error && !execution.signal && [0, 1].includes(execution.status),
    `checker did not finish normally (status=${execution.status}, signal=${execution.signal})`);
  let envelope;
  try { envelope = JSON.parse(execution.stdout); }
  catch { throw new Error('checker measurement refused: missing or malformed JSON report'); }
  requireReport(envelope?.schemaVersion === 1 && typeof envelope.ok === 'boolean'
    && Array.isArray(envelope.warnings), 'unsupported check envelope');
  requireReport(envelope.ok === (execution.status === 0), 'exit status disagrees with report');
  requireReport(envelope.ok ? envelope.error === null : envelope.error?.code === 'check_failed',
    'checker failed before producing file results');
  const report = envelope.data;
  requireReport(Array.isArray(report?.files) && report.files.length > 0, 'no files measured');
  // This repository gate covers every committed stdlib source, including a
  // source omitted from the embedded catalog. Do not accept a partial census.
  const expected = new Set(execFileSync('git', ['ls-files', '-z', '--', directory], {
    encoding: 'utf8', timeout: 10_000, maxBuffer: 1024 * 1024,
  }).split('\0').filter(path => path.endsWith('.harn') || path.endsWith('.harn.txt')));
  requireReport(expected.size > 0, 'no committed stdlib sources');
  const measured = { ok: 0, warnings: 0, errors: 0, diagnostics: 0 };
  const paths = new Set();
  const failures = [];
  for (const file of report.files) {
    requireReport(typeof file.path === 'string' && file.path.length > 0, 'missing file path');
    const path = relative(process.cwd(), resolve(file.path)).split(sep).join('/');
    requireReport(path.startsWith(`${directory}/`) && !paths.has(path), 'foreign or duplicate file');
    paths.add(path);
    requireReport(Array.isArray(file.diagnostics), `missing diagnostics for ${path}`);
    let status = 'ok';
    for (const diagnostic of file.diagnostics) {
      requireReport(['info', 'warning', 'error'].includes(diagnostic.severity)
        && typeof diagnostic.source === 'string' && diagnostic.source.length > 0
        && typeof diagnostic.message === 'string'
        && (diagnostic.code === undefined || (typeof diagnostic.code === 'string' && diagnostic.code.length > 0))
        && (diagnostic.span === undefined || (count(diagnostic.span?.start)
          && count(diagnostic.span?.end) && diagnostic.span.end >= diagnostic.span.start)),
      `invalid diagnostic for ${path}`);
      if (diagnostic.severity === 'error') status = 'error';
      else if (diagnostic.severity === 'warning' && status === 'ok') status = 'warning';
      const typeError = diagnostic.severity === 'error' && diagnostic.code?.startsWith('HARN-TYP-');
      const otherError = diagnostic.severity === 'error' && diagnostic.source !== 'lint';
      const ownership = diagnostic.code === 'HARN-OWN-004' && !frontier.has(path);
      if (typeError || otherError || ownership) {
        failures.push(`${path}: ${diagnostic.code ?? diagnostic.source}: ${diagnostic.message}`);
      }
    }
    requireReport(file.status === status, `file status disagrees with diagnostics for ${path}`);
    measured[status === 'ok' ? 'ok' : `${status}s`] += 1;
    measured.diagnostics += file.diagnostics.length;
  }
  requireReport(Object.entries(measured).every(([key, value]) => count(report.summary?.[key])
    && report.summary[key] === value), 'summary disagrees with measured files');
  requireReport(paths.size === expected.size && [...expected].every(path => paths.has(path)),
    `partial stdlib census (${paths.size} reported, ${expected.size} committed sources)`);
  requireReport(!envelope.ok || measured.errors === 0, 'successful checker reports errors');
  requireReport(envelope.ok || measured.errors + measured.warnings > 0,
    'failed checker reports no failure findings');
  if (failures.length > 0) {
    throw new Error(`stdlib type-safety gate failed: ${failures.length} findings across ${paths.size} measured files:\n  ${failures.join('\n  ')}`);
  }
  console.log(`stdlib type-safety gate passed: ${paths.size} measured files, ${measured.diagnostics} diagnostics, 0 type or unratcheted boundary findings.`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
