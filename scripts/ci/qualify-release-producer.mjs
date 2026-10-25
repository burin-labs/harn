import {githubArchiveProducer,observerRefusal} from '../lib/consumer-observer.mjs';

try {
  if(process.env.GITHUB_REPOSITORY!=='burin-labs/harn') throw new Error('CONSUMER_OBSERVER reason=archive_producer_identity_unproven');
  const proof=await githubArchiveProducer(process.env.GH_TOKEN,{
    sourceRevision:process.env.EXPECTED_SOURCE_SHA,runId:process.env.CANDIDATE_RUN_ID,
    attempt:Number(process.env.CANDIDATE_RUN_ATTEMPT),
  });
  console.log(JSON.stringify(proof));
} catch(error) {
  console.error(`::error::${observerRefusal(error,'archive_producer_unmeasured').line}`);
  process.exitCode=1;
}
