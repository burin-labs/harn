import fs from 'node:fs';
import path from 'node:path';
import {
  createTerminalReceipt, githubArtifactCensus, githubArtifactJson,
  githubDeclaration, githubRecord, observerContext, observerTerminalName,
} from '../lib/consumer-observer.mjs';

async function main() {
  const env = process.env;
  if (!env.OBSERVER_CONTEXT) {
    fs.appendFileSync(env.GITHUB_OUTPUT, 'recorded=false\n');
    console.log('CONSUMER_OBSERVER product=ordinary');
    return;
  }
  const context = observerContext(JSON.parse(env.OBSERVER_CONTEXT));
  const prefix = `${env.GITHUB_REPOSITORY}/`;
  if (!env.GITHUB_WORKFLOW_REF?.startsWith(prefix)) {
    throw new Error('CONSUMER_OBSERVER reason=workflow_identity_unproven');
  }
  const expected = {
    repository: env.GITHUB_REPOSITORY,
    workflow: env.GITHUB_WORKFLOW_REF.slice(prefix.length).split('@')[0],
  };
  const name = `consumer-observer-${context.dispatch_nonce}`;
  const artifacts = await githubArtifactCensus(env.GH_TOKEN, expected.repository,
    env.GITHUB_RUN_ID, fetch, name);
  if (artifacts.length !== 1 || artifacts[0].name !== name) {
    throw new Error('CONSUMER_OBSERVER reason=child_receipt_missing_or_ambiguous');
  }
  const ownership = await githubArtifactJson(env.GH_TOKEN, expected.repository, artifacts[0], {
    runId: env.GITHUB_RUN_ID, headSha: env.GITHUB_SHA,
    name, filename: `${name}.json`,
  });
  const declaration = await githubDeclaration(env.GH_TOKEN, context);
  const [parent, child] = await Promise.all([
    githubRecord(env.GH_TOKEN, context.parent_repository, context.parent_run_id,
      fetch, context.parent_run_attempt),
    githubRecord(env.GH_TOKEN, expected.repository, env.GITHUB_RUN_ID),
  ]);
  if (String(child.id) !== env.GITHUB_RUN_ID || child.run_attempt !== Number(env.GITHUB_RUN_ATTEMPT)
    || child.head_sha !== env.GITHUB_SHA) {
    throw new Error('CONSUMER_OBSERVER reason=producer_attempt_unproven');
  }
  const receipt = createTerminalReceipt(ownership, context, parent, child, expected,
    declaration, JSON.parse(fs.readFileSync(env.PRODUCT_DECISION_FILE, 'utf8')));
  const artifact = observerTerminalName(context);
  const directory = path.join(env.RUNNER_TEMP, artifact);
  fs.mkdirSync(directory, {mode:0o700});
  const file = path.join(directory, `${name}.json`);
  fs.writeFileSync(file, `${JSON.stringify(receipt)}\n`, {flag:'wx',mode:0o600});
  fs.appendFileSync(env.GITHUB_OUTPUT, `recorded=true\nreceipt=${file}\nartifact=${artifact}\n`);
  console.log(`CONSUMER_OBSERVER product=recorded owed=${receipt.product.owed} proven=${receipt.product.proven} pending=${receipt.product.pending} failed=${receipt.product.failed}`);
}

main().catch(error => {
  const reason = /^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1] ?? 'product_decision_unmeasured';
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode = 1;
});
