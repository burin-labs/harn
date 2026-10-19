import fs from 'node:fs';

/** Time bounds do not grant child ownership or a product verdict. Callers must
 * first authenticate the complete job census for the original child attempt. */
const policy = JSON.parse(fs.readFileSync(new URL('../ci/consumer_observation_budget.json', import.meta.url), 'utf8'));
if (Object.keys(policy).sort().join(',') !== 'cleanup_seconds,execution_seconds,poll_seconds,queue_seconds,wall_seconds,window_seconds'
  || !Object.values(policy).every(value => Number.isSafeInteger(value) && value > 0)
  || policy.wall_seconds !== policy.queue_seconds + policy.execution_seconds
  || policy.poll_seconds > policy.window_seconds || policy.window_seconds > 2700
  || policy.window_seconds >= policy.wall_seconds || policy.cleanup_seconds < policy.poll_seconds) {
  throw new Error('CONSUMER_OBSERVER reason=observation_budget_invalid');
}
export const consumerObservationBudget = Object.freeze(policy);

export function unmeasuredConsumerTiming(startedAt, now, discovered) {
  if (!Number.isSafeInteger(startedAt) || !Number.isSafeInteger(now) || now < startedAt
    || !Number.isSafeInteger(discovered) || discovered < 0) refuse();
  const wall = now - startedAt;
  return {reason: wall >= consumerObservationBudget.queue_seconds ? 'queue_deadline' : null,
    discovered, owned: 0, observed_jobs: null, queued_jobs: null, executing_jobs: null,
    queue_seconds: null, execution_seconds: null, wall_seconds: wall};
}

function refuse() {
  throw new Error('CONSUMER_OBSERVER reason=job_timing_unmeasured');
}

function timestamp(value) {
  if (typeof value !== 'string' || !value) refuse();
  const parsed = Date.parse(value) / 1000;
  if (!Number.isSafeInteger(parsed)) refuse();
  return parsed;
}

/** A queued job's started_at can equal its creation time before allocation.
 * Only an allocated runner and a reached step establish execution. */
function executionStart(job, created, now) {
  if (!['in_progress', 'completed'].includes(job.status)) return null;
  if (!Number.isSafeInteger(job.runner_id) || job.runner_id < 1
    || typeof job.runner_name !== 'string' || !job.runner_name
    || !Array.isArray(job.steps)) return null;
  const reached = job.steps.filter(step => ['in_progress', 'completed'].includes(step.status)
    && step.conclusion !== 'skipped');
  if (reached.length === 0) return null;
  const started = timestamp(job.started_at);
  const firstStep = Math.min(...reached.map(step => timestamp(step.started_at)));
  if (started < created || firstStep < started || firstStep > now) refuse();
  return started;
}

export function assessConsumerObservation(startedAt, child, jobs, now,
  policy = consumerObservationBudget) {
  if (!Number.isSafeInteger(startedAt) || !Number.isSafeInteger(now)
    || startedAt < 0 || now < startedAt || !Number.isSafeInteger(child?.id)
    || !Number.isSafeInteger(child.run_attempt) || child.run_attempt < 1
    || !/^[0-9a-f]{40}$/.test(child.head_sha ?? '')
    || !Array.isArray(jobs) || jobs.length === 0) refuse();
  const ids = new Set();
  let maxQueue = 0, maxExecution = 0, queued = 0, executing = 0;
  for (const job of jobs) {
    if (!Number.isSafeInteger(job.id) || job.id < 1 || ids.has(job.id)
      || job.run_id !== child.id || job.run_attempt !== child.run_attempt
      || job.head_sha !== child.head_sha
      || !['queued', 'requested', 'waiting', 'pending', 'in_progress', 'completed'].includes(job.status)) refuse();
    ids.add(job.id);
    if (job.status === 'completed' && job.conclusion === 'skipped') continue;
    const created = timestamp(job.created_at);
    if (created < startedAt || created > now) refuse();
    const end = job.status === 'completed' ? timestamp(job.completed_at) : now;
    if (end < created || end > now) refuse();
    const start = executionStart(job, created, end);
    if (start === null) {
      if (job.status !== 'completed') queued++;
      maxQueue = Math.max(maxQueue, end - created);
      continue;
    }
    if (end < start) refuse();
    maxQueue = Math.max(maxQueue, start - created);
    maxExecution = Math.max(maxExecution, end - start);
    if (job.status !== 'completed') executing++;
  }
  const wall = now - startedAt;
  const reason = wall >= policy.wall_seconds ? 'aggregate_deadline'
    : maxQueue >= policy.queue_seconds ? 'queue_deadline'
      : maxExecution >= policy.execution_seconds ? 'execution_deadline' : null;
  return {reason, observed_jobs: jobs.length, queued_jobs: queued, executing_jobs: executing,
    queue_seconds: maxQueue, execution_seconds: maxExecution, wall_seconds: wall};
}
