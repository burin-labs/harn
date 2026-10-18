import fs from 'node:fs';
import {retireCandidateObservers} from '../lib/consumer-observer-retirement.mjs';

async function main() {
  const env = process.env;
  const event = JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8'));
  if (env.GITHUB_EVENT_NAME !== 'pull_request_target' || env.GITHUB_REPOSITORY !== 'burin-labs/harn'
    || event?.repository?.full_name !== env.GITHUB_REPOSITORY || event.action !== 'closed') {
    throw new Error('CONSUMER_OBSERVER reason=candidate_event_unproven');
  }
  const result = await retireCandidateObservers(env.GH_TOKEN, event.number);
  console.log(`CONSUMER_OBSERVER retirement=measured ${Object.entries(result).map(([key,value])=>`${key}=${value}`).join(' ')}`);
  if (result.pending > 0 || result.refused > 0) process.exitCode = 1;
}

main().catch(error => {
  const reason = /^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1] ?? 'candidate_retirement_unmeasured';
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode = 1;
});
