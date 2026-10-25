import {githubWorkflowRuns, githubRecord, observerParentWorkflows, observerRunIsLive,
  observerParentRepository as repository} from './consumer-observer.mjs';

/** Retire only PR-triggered parents of the authenticated closed candidate.
 * Their completion listener remains the sole owner of child cancellation. */
export async function retireCandidateObservers(token, number, fetchRecord = fetch) {
  if (!token || !Number.isSafeInteger(number) || number < 1) {
    throw new Error('CONSUMER_OBSERVER reason=candidate_identity_unproven');
  }
  const headers = {Authorization:`Bearer ${token}`, Accept:'application/vnd.github+json'};
  const response = await fetchRecord(`https://api.github.com/repos/${repository}/pulls/${number}`, {
    headers, signal:AbortSignal.timeout(30_000),
  });
  if (!response.ok) throw new Error('CONSUMER_OBSERVER reason=candidate_read_refused');
  const pull = await response.json();
  if (pull?.number !== number || pull.base?.repo?.full_name !== repository) {
    throw new Error('CONSUMER_OBSERVER reason=candidate_identity_unproven');
  }
  // Observers are same-repository PR runs of a closed candidate: a reopened PR or a fork/deleted
  // head has nothing to retire, so it is a no-op rather than a failure.
  // Only an explicit open state, a null (deleted) head, or a named foreign head proves a no-op.
  if (pull.state === 'open') return {noop:'pull_request_not_closed'};
  if (pull.state !== 'closed') throw new Error('CONSUMER_OBSERVER reason=candidate_identity_unproven');
  if (pull.head?.repo === null) return {noop:'head_repository_deleted'};
  const headRepository = pull.head?.repo?.full_name;
  if (typeof headRepository !== 'string' || !headRepository) {
    throw new Error('CONSUMER_OBSERVER reason=candidate_identity_unproven');
  }
  if (headRepository !== repository) return {noop:'head_not_same_repository'};
  if (!/^[0-9a-f]{40}$/.test(pull.head?.sha ?? '')
    || typeof pull.head?.ref !== 'string' || !pull.head.ref) {
    throw new Error('CONSUMER_OBSERVER reason=candidate_identity_unproven');
  }
  const result = {discovered:0, cancelled:0, terminal:0, pending:0, refused:0};
  for (const workflow of [observerParentWorkflows.candidate, observerParentWorkflows.pull_request]) {
    const filename = workflow.slice('.github/workflows/'.length);
    const runs = await githubWorkflowRuns(token, repository, filename,
      {event:'pull_request', head_sha:pull.head.sha}, fetchRecord);
    for (const candidate of runs) {
      const run = await githubRecord(token, repository, String(candidate.id), fetchRecord);
      if (run.event !== 'pull_request' || run.head_sha !== pull.head.sha || run.head_branch !== pull.head.ref
        || run.head_repository?.full_name !== repository || run.path !== workflow) continue;
      result.discovered += 1;
      if (run.status === 'completed') {
        result.terminal += 1;
      } else if (observerRunIsLive(run.status)) {
        const refreshed = await fetchRecord(`https://api.github.com/repos/${repository}/pulls/${number}`, {
          headers, signal:AbortSignal.timeout(30_000),
        });
        const current = refreshed.ok ? await refreshed.json() : null;
        if (current?.number !== number || current.state !== 'closed' || current.head?.sha !== pull.head.sha
          || current.head?.ref !== pull.head.ref || current.head?.repo?.full_name !== repository) {
          result.refused += 1;
          continue;
        }
        const cancelled = await fetchRecord(`https://api.github.com/repos/${repository}/actions/runs/${run.id}/cancel`, {
          method:'POST', headers, signal:AbortSignal.timeout(30_000),
        });
        if (cancelled.status === 202) result.cancelled += 1;
        else result.refused += 1;
      } else {
        result.pending += 1;
      }
    }
  }
  return result;
}
