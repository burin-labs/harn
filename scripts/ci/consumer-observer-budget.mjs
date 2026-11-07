import {observeConsumerTiming, observerContext, parentHeadSha, observerRefusal, observerReadCredentials, observerReadMetrics, observerReadRefusal } from '../lib/consumer-observer.mjs';
import {consumerObservationBudget} from '../lib/consumer-observation-budget.mjs';
import {consumerTimingCache} from '../lib/consumer-timing-cache.mjs';
import {randomBytes} from 'node:crypto';

try {
  if (process.argv.length === 3 && process.argv[2] === '--cache-key') {
    process.stdout.write(randomBytes(32).toString('hex'));
  } else if (process.argv.length === 3 && process.argv[2] === '--policy') {
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
    const credentials = observerReadCredentials(env.GITHUB_TOKEN, env.GH_TOKEN,
      context.parent_repository, env.CANARY_REPOSITORY);
    const expected = {repository: env.CANARY_REPOSITORY, workflow: `.github/workflows/${env.CANARY_WORKFLOW}`,
      runId: env.CANARY_RUN_ID};
    const cache = env.CANARY_CHECKPOINT_FILE && env.CANARY_TIMING_CACHE_KEY
      ? consumerTimingCache(`${env.CANARY_CHECKPOINT_FILE}.timing`, context, expected, env.CANARY_TIMING_CACHE_KEY) : null;
    const reading = await observeConsumerTiming(credentials, context, expected, undefined, undefined, undefined, cache);
    console.log(JSON.stringify({...reading, read_attempts:observerReadMetrics(credentials)}));
  }
} catch (error) {
  const {line} = observerRefusal(error, 'job_timing_unmeasured');
  const refusal=observerReadRefusal(error,'job_timing_unmeasured');
  if (refusal.retryable || refusal.status !== undefined) {
    console.log(JSON.stringify(refusal));
    console.error(`${line} retryable=${refusal.retryable}`);
    process.exitCode = refusal.retryable ? 75 : 1;
  } else {
    console.error(`::error::${line}`);
    process.exitCode = 1;
  }
}
