import fs from 'node:fs';
import {consumerProductVerdict} from './consumer-observer.mjs';

/** One budget owns observation, credential windows and workflow projections. */
export function consumerObservationPolicy() {
  const policy=JSON.parse(fs.readFileSync(new URL('../ci/consumer_canary_policy.json',import.meta.url),'utf8'));
  if(Object.keys(policy).sort().join(',')!=='deadline_seconds,poll_seconds,window_seconds'
    || !Object.values(policy).every(value=>Number.isSafeInteger(value)&&value>0)
    || policy.poll_seconds>policy.window_seconds || policy.window_seconds>=policy.deadline_seconds){
    throw new Error('CONSUMER_OBSERVER reason=observation_budget_invalid');
  }
  return {...policy,max_windows:Math.ceil(policy.deadline_seconds/policy.window_seconds)};
}

/** Renewing credentials resumes the same context and aggregate deadline.
 * Observation owns custody and product authority; this loop owns time only. */
export async function waitConsumerWindow(startedAt, observe, {
  now=()=>Math.floor(Date.now()/1000),
  pause=seconds=>new Promise(resolve=>setTimeout(resolve,seconds*1000)),
  policy=consumerObservationPolicy(),
}={}) {
  const windowStarted=now();
  if(!Number.isSafeInteger(startedAt)||startedAt<0||startedAt>windowStarted){
    throw new Error('CONSUMER_OBSERVER reason=observation_start_invalid');
  }
  let lastReading=null;
  let previous=windowStarted;
  for(;;){
    const current=now();
    if(!Number.isSafeInteger(current)||current<previous){
      throw new Error('CONSUMER_OBSERVER reason=observation_clock_invalid');
    }
    const wallSeconds=current-startedAt;
    if(wallSeconds>=policy.deadline_seconds) return {verdict:'deadline',wallSeconds,reading:lastReading};
    lastReading=await observe();
    const afterRead=now();
    if(!Number.isSafeInteger(afterRead)||afterRead<current){
      throw new Error('CONSUMER_OBSERVER reason=observation_clock_invalid');
    }
    previous=afterRead;
    if(afterRead-startedAt>=policy.deadline_seconds) return {verdict:'deadline',wallSeconds:afterRead-startedAt,reading:lastReading};
    const verdict=consumerProductVerdict(lastReading);
    if(verdict!=='pending') return {verdict,wallSeconds:afterRead-startedAt,reading:lastReading};
    if(afterRead-windowStarted>=policy.window_seconds) return {verdict:'pending',wallSeconds:afterRead-startedAt,reading:lastReading};
    await pause(Math.min(policy.poll_seconds,policy.window_seconds-(afterRead-windowStarted),
      policy.deadline_seconds-(afterRead-startedAt)));
  }
}
