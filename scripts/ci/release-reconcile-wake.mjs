import { appendFileSync, readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const failed = new Set(['failure', 'cancelled', 'timed_out', 'startup_failure']);
const producerEvents = new Set(['push', 'schedule', 'workflow_dispatch']);

// A completed producer is only a wake-up. The existing reconciler still reads
// main, candidate and promotion state before requesting an exact-source build.
export function shouldReconcile({ eventName, event, repository, planResult, planReconcile }) {
  if (planResult === 'success' && planReconcile === 'true') return true;
  const run = event?.workflow_run;
  return eventName === 'workflow_run'
    && event?.repository?.full_name === repository
    && run?.repository?.full_name === repository
    && run?.head_repository?.full_name === repository
    && run?.path === '.github/workflows/build-release-binaries.yml'
    && run?.head_branch === 'main'
    && producerEvents.has(run?.event)
    && run?.status === 'completed'
    && failed.has(run?.conclusion)
    && Number.isSafeInteger(run?.id) && run.id > 0
    && Number.isSafeInteger(run?.run_attempt) && run.run_attempt > 0
    && typeof run?.head_sha === 'string' && /^[0-9a-f]{40}$/.test(run.head_sha)
    && run.head_sha !== '0'.repeat(40);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
  const required = shouldReconcile({
    eventName: process.env.GITHUB_EVENT_NAME,
    event,
    repository: process.env.GITHUB_REPOSITORY,
    planResult: process.env.PLAN_RESULT,
    planReconcile: process.env.PLAN_RECONCILE,
  });
  appendFileSync(process.env.GITHUB_OUTPUT, `required=${required}\n`);
}
