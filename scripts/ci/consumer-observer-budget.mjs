import {observeConsumerTiming, observerContext, parentHeadSha, observerRefusal, observerReadRetryable } from '../lib/consumer-observer.mjs';
import {consumerObservationBudget} from '../lib/consumer-observation-budget.mjs';

try {
  if (process.argv.length === 3 && process.argv[2] === '--policy') {
    const policy = consumerObservationBudget;
    console.log(JSON.stringify({...policy, budget_version: 2,
      deadline_seconds: policy.wall_seconds,
      max_windows: Math.ceil(policy.wall_seconds / policy.window_seconds),
      parent_timeout_minutes: Math.ceil((policy.wall_seconds + policy.cleanup_seconds) / 60)}));
  } else {
    if (process.argv.length !== 2) throw new Error('CONSUMER_OBSERVER reason=observation_budget_invalid');
    const env = process.env;
    const context = observerContext(JSON.parse(env.OBSERVER_CONTEXT));
    if (context.parent_repository !== env.GITHUB_REPOSITORY
      || context.parent_run_id !== env.GITHUB_RUN_ID
      || context.parent_run_attempt !== Number(env.GITHUB_RUN_ATTEMPT)
      || context.parent_head_sha !== parentHeadSha(env)) {
      throw new Error('CONSUMER_OBSERVER reason=observer_invocation_unproven');
    }
    const reading = await observeConsumerTiming(env.GH_TOKEN, context,
      {repository: env.CANARY_REPOSITORY, workflow: `.github/workflows/${env.CANARY_WORKFLOW}`,
        runId: env.CANARY_RUN_ID});
    console.log(JSON.stringify(reading));
  }
} catch (error) {
  const {line, reason} = observerRefusal(error, 'job_timing_unmeasured');
  if (observerReadRetryable(error)) {
    console.log(JSON.stringify({schema:'harn.consumer_observer.read_refusal.v1',reason,retryable:true,
      retry_after_seconds:error?.diagnostic?.retry_after_seconds ?? 0}));
    console.error(`${line} retryable=true`);
    process.exitCode = 75;
  } else {
    console.error(`::error::${line}`);
    process.exitCode = 1;
  }
}
