import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {githubWorkflowRuns} from './consumer-observer.mjs';

/** @typedef {{depth:number|null, pending_runs:{run_id:string, run_attempt:number}[]|null}} ConsumerQueue */
/** @typedef {{schema:string, reason:string, run_id:string, queue:ConsumerQueue}} ConsumerFailure */

/** A bounded failure descriptor carries numeric identities, never names or logs.
 * An unavailable census remains null, never zero.
 * @returns {Promise<ConsumerQueue>} */
export async function consumerQueue(token, repository, workflow, fetchRecord = fetch) {
  try {
    const runs = await githubWorkflowRuns(token, repository, workflow, {status:'queued'}, fetchRecord);
    if (runs.some(run => run.status !== 'queued' || !Number.isSafeInteger(run.run_attempt) || run.run_attempt < 1)) {
      throw new Error('unmeasured queue identity');
    }
    return {depth:runs.length, pending_runs:runs.map(run=>({run_id:String(run.id), run_attempt:run.run_attempt}))};
  } catch {
    return {depth:null, pending_runs:null};
  }
}

/** @returns {Promise<ConsumerFailure>} */
export async function writeConsumerFailure(env, reason, fetchRecord = fetch) {
  if (!/^[a-z_]+$/.test(reason) || !/^[1-9][0-9]*$/.test(env.CANARY_RUN_ID ?? '')) {
    throw new Error('CONSUMER_OBSERVER reason=failure_identity_unproven');
  }
  const queue = await consumerQueue(env.GH_TOKEN, env.CANARY_REPOSITORY, env.CANARY_WORKFLOW, fetchRecord);
  const receipt = {schema:'harn.consumer_observer.failure.v1', reason, run_id:env.CANARY_RUN_ID, queue};
  const directory = fs.mkdtempSync(path.join(env.RUNNER_TEMP || os.tmpdir(), 'consumer-observer-failure-'));
  fs.chmodSync(directory, 0o700);
  const file = path.join(directory, 'failure.json');
  fs.writeFileSync(file, JSON.stringify(receipt), {mode:0o600});
  if (env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT, `failure_receipt=${file}\n`);
  return receipt;
}
