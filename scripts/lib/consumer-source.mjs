export const consumerCompositionPath = '.github/harn-consumer-composition.json';
export const consumerCompositionSchema = 'harn.consumer_composition.v1';

const revision = /^[0-9a-f]{40}$/;
const pullNumber = /^[1-9][0-9]*$/;
const repositoryName = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const componentLimit = 64;
const graphLimit = 1000;
const manifestLimit = 32 * 1024;

function safePull(value) {
  return typeof value === 'string' && pullNumber.test(value)
    && Number.isSafeInteger(Number(value));
}

function canonicalPulls(values) {
  return Array.isArray(values) && values.length > 0 && values.length <= componentLimit
    && values.every(safePull) && new Set(values).size === values.length;
}

function refuse(reason = 'consumer_composition_manifest_unqualified', pulls) {
  const error = new Error(`CONSUMER_OBSERVER reason=${reason}`);
  if (pulls) error.pulls = pulls;
  throw error;
}

function closed(record, names) {
  return record !== null && typeof record === 'object' && !Array.isArray(record)
    && Object.keys(record).sort().join(',') === [...names].sort().join(',');
}

/** Decode private composition data, without granting source-selection authority.
 * The authenticated reader must separately prove the base and every component.
 */
export function decodeConsumerCompositionManifest(value, context) {
  if (!closed(context, ['repository', 'requiredPulls', 'compositionPull'])
    || typeof context.repository !== 'string' || !repositoryName.test(context.repository)
    || !safePull(context.compositionPull) || !canonicalPulls(context.requiredPulls)) refuse();
  if (!closed(value, ['schema', 'repository', 'base_sha', 'adaptations', 'repairs'])
    || value.schema !== consumerCompositionSchema || value.repository !== context.repository
    || typeof value.base_sha !== 'string' || !revision.test(value.base_sha)
    || !Array.isArray(value.adaptations) || !Array.isArray(value.repairs)
    || value.adaptations.length === 0
    || value.adaptations.length + value.repairs.length > componentLimit) refuse();

  const seen = new Set();
  const decode = components => Object.freeze(components.map(component => {
    if (!closed(component, ['pull_request', 'head_sha'])
      || !Number.isSafeInteger(component.pull_request) || component.pull_request < 1
      || typeof component.head_sha !== 'string' || !revision.test(component.head_sha)) refuse();
    const pull = String(component.pull_request);
    if (seen.has(pull) || pull === context.compositionPull) refuse();
    seen.add(pull);
    return Object.freeze({pull_request: component.pull_request, head_sha: component.head_sha});
  }).sort((left, right) => left.pull_request - right.pull_request));

  const adaptations = decode(value.adaptations);
  const repairs = decode(value.repairs);
  const required = [...context.requiredPulls].sort();
  const declared = adaptations.map(component => String(component.pull_request)).sort();
  if (required.length !== declared.length || required.some((pull, index) => pull !== declared[index])) refuse();

  return Object.freeze({
    schema: consumerCompositionSchema,
    repository: context.repository,
    base_sha: value.base_sha,
    adaptations,
    repairs,
  });
}

/** Public source expresses intent; this result is not an authenticated source. */
export function consumerSourceDeclaration(text, requestedPull = '') {
  if (typeof requestedPull !== 'string' || (requestedPull !== '' && !pullNumber.test(requestedPull))) {
    refuse('consumer_pairing_invalid');
  }
  const pairs = new Set();
  const compositions = new Set();
  for (const line of String(text ?? '').split(/\r?\n/)) {
    if (/^\s*Pairs-with:/.test(line)) {
      const match = /^\s*Pairs-with:\s*consumer#([1-9][0-9]*)\s*$/.exec(line);
      if (!match) refuse('consumer_pairing_invalid');
      pairs.add(match[1]);
    } else if (/^\s*Consumer-composition:/.test(line)) {
      const match = /^\s*Consumer-composition:\s*consumer#([1-9][0-9]*)\s*$/.exec(line);
      if (!match) refuse('consumer_composition_declaration_invalid');
      compositions.add(match[1]);
    }
  }
  const sorted = values => [...values].sort((left, right) => Number(left) - Number(right));
  if (compositions.size > 1) refuse('consumer_composition_declaration_ambiguous');
  if (compositions.size === 1) {
    const pull = [...compositions][0];
    if (pairs.size === 0 || (requestedPull !== '' && requestedPull !== pull) || pairs.has(pull)) {
      refuse('consumer_composition_declaration_invalid');
    }
    return Object.freeze({kind: 'composition', pull, requiredPulls: Object.freeze(sorted(pairs))});
  }
  if (requestedPull !== '') pairs.add(requestedPull);
  if (pairs.size > 1) refuse('consumer_pairing_ambiguous', sorted(pairs));
  return Object.freeze({kind: 'ordinary', pull: pairs.size === 1 ? [...pairs][0] : null});
}

/** The scalar adapter cannot admit a composition without authenticated reads. */
export function consumerPairing(text, requestedPull = '') {
  const declaration = consumerSourceDeclaration(text, requestedPull);
  if (declaration.kind !== 'ordinary') refuse('consumer_composition_authentication_required');
  return declaration.pull;
}

export function consumerSourceDeclarationValue(value) {
  if (closed(value, ['kind', 'pull']) && value.kind === 'ordinary'
    && (value.pull === null || safePull(value.pull))) {
    return Object.freeze({kind: 'ordinary', pull: value.pull});
  }
  if (!closed(value, ['kind', 'pull', 'requiredPulls']) || value.kind !== 'composition'
    || !safePull(value.pull) || !canonicalPulls(value.requiredPulls) || value.requiredPulls.includes(value.pull)) {
    refuse('consumer_composition_declaration_invalid');
  }
  return Object.freeze({kind: 'composition', pull: value.pull,
    requiredPulls: Object.freeze([...value.requiredPulls].sort((a, b) => Number(a) - Number(b)))});
}

export function consumerSourceEnvironment(env) {
  if (!env.CANARY_SOURCE_DECLARATION) return consumerSourceDeclaration('', env.CANARY_PAIRED_PULL || '');
  let value;
  try { value = JSON.parse(env.CANARY_SOURCE_DECLARATION); } catch { refuse('consumer_composition_declaration_invalid'); }
  const declaration = consumerSourceDeclarationValue(value);
  if (declaration.pull !== (env.CANARY_PAIRED_PULL || null)) refuse('consumer_composition_declaration_invalid');
  return declaration;
}
import {createHash} from 'node:crypto';
import {TextDecoder} from 'node:util';


export function consumerCompositionIdentity(value) {
  if (!closed(value, ['schema', 'pull', 'requiredPulls', 'manifestSha256'])
    || value.schema !== 'harn.consumer_composition_identity.v1' || !safePull(value.pull)
    || !canonicalPulls(value.requiredPulls) || value.requiredPulls.includes(value.pull)
    || !/^[0-9a-f]{64}$/.test(value.manifestSha256 ?? '')) {
    refuse('consumer_composition_identity_unqualified');
  }
  return Object.freeze({schema: value.schema, pull: value.pull,
    requiredPulls: Object.freeze([...value.requiredPulls].sort((a, b) => Number(a) - Number(b))),
    manifestSha256: value.manifestSha256});
}

/** The reader supplies authenticated same-repository API responses. A manifest
 * describes the intended graph; it cannot authorize a missing component. */
export async function resolveConsumerComposition(repository, declaration, read) {
  if (!repositoryName.test(repository ?? '')
    || !closed(declaration, ['kind', 'pull', 'requiredPulls']) || declaration.kind !== 'composition'
    || !safePull(declaration.pull) || !canonicalPulls(declaration.requiredPulls)
    || declaration.requiredPulls.includes(declaration.pull)) refuse('consumer_composition_declaration_invalid');
  const pullNumber = declaration.pull;
  const requiredPulls = [...declaration.requiredPulls];
  let reads = 0;
  const record = async suffix => {
    if (++reads > 256) refuse('consumer_composition_graph_unmeasured');
    const response = await read(suffix);
    if (!response || typeof response.more !== 'boolean') refuse('consumer_composition_graph_unmeasured');
    return response;
  };
  const one = async suffix => {
    const response = await record(suffix);
    if (response.more) refuse('consumer_composition_graph_unmeasured');
    return response.value;
  };
  const repo = await one('');
  if (repo?.full_name !== repository || typeof repo.default_branch !== 'string' || !repo.default_branch) {
    refuse('consumer_source_unmeasured');
  }
  const pull = async (number, selected = false) => {
    const value = await one(`/pulls/${number}`);
    if (value?.number !== Number(number) || value.base?.repo?.full_name !== repository
      || value.base.ref !== repo.default_branch || value.head?.repo?.full_name !== repository
      || typeof value.head.ref !== 'string' || !value.head.ref || !revision.test(value.head.sha ?? '')
      || (selected ? value.state !== 'open' || value.merged !== false
        : !(value.state === 'open' && value.merged === false || value.state === 'closed' && value.merged === true))) {
      refuse('consumer_composition_pull_unqualified');
    }
    return {headSha: value.head.sha, branch: value.head.ref, state: value.state, merged: value.merged};
  };
  const selected = await pull(pullNumber, true);
  const manifestResponse = await one(`/contents/${consumerCompositionPath}?ref=${selected.headSha}`);
  if (manifestResponse?.type !== 'file' || manifestResponse.path !== consumerCompositionPath
    || manifestResponse.encoding !== 'base64' || !revision.test(manifestResponse.sha ?? '')
    || !Number.isSafeInteger(manifestResponse.size) || manifestResponse.size < 1
    || manifestResponse.size > manifestLimit || typeof manifestResponse.content !== 'string'
    || manifestResponse.content.length > manifestLimit * 2) refuse();
  const encoded = manifestResponse.content.replace(/\n/g, '');
  const bytes = Buffer.from(encoded, 'base64');
  if (bytes.length !== manifestResponse.size || bytes.toString('base64') !== encoded
    || createHash('sha1').update(`blob ${bytes.length}\0`).update(bytes).digest('hex') !== manifestResponse.sha) refuse();
  let raw;
  try { raw = JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(bytes)); } catch { refuse(); }
  const manifest = decodeConsumerCompositionManifest(raw, {repository, requiredPulls, compositionPull: pullNumber});
  const components = [...manifest.adaptations, ...manifest.repairs];
  const snapshots = [];
  for (const component of components) {
    const snapshot = await pull(String(component.pull_request));
    if (snapshot.headSha !== component.head_sha) refuse('consumer_composition_component_moved');
    snapshots.push(snapshot);
  }

  const comparison = async (base, head, ancestor = false) => {
    const commits = [];
    let total;
    for (let page = 1; total === undefined || commits.length < total; page++) {
      const result = await record(`/compare/${base}...${head}?per_page=100&page=${page}`);
      const value = result.value;
      if (value?.base_commit?.sha !== base || !revision.test(value.merge_base_commit?.sha ?? '')
        || !['ahead', 'behind', 'diverged', 'identical'].includes(value.status)
        || ancestor && (!['ahead', 'identical'].includes(value.status) || value.merge_base_commit.sha !== base)
        || !Number.isSafeInteger(value.total_commits) || value.total_commits < 0 || value.total_commits > graphLimit
        || value.status === 'identical' && (base !== head || value.total_commits !== 0 || value.merge_base_commit.sha !== base)
        || value.status === 'ahead' && (base === head || value.total_commits < 1 || value.merge_base_commit.sha !== base)
        || value.status === 'behind' && (base === head || value.total_commits !== 0 || value.merge_base_commit.sha !== head)
        || value.status === 'diverged' && (value.total_commits < 1 || [base, head].includes(value.merge_base_commit.sha))
        || ancestor && (base === head
          ? value.status !== 'identical' || value.total_commits !== 0
          : value.status !== 'ahead' || value.total_commits < 1)
        || total !== undefined && total !== value.total_commits || !Array.isArray(value.commits)) {
        refuse('consumer_composition_ancestry_unproven');
      }
      total = value.total_commits;
      const remaining = total - commits.length;
      if (value.commits.length !== Math.min(100, remaining) || result.more !== (remaining > 100)
        || value.commits.some(commit => !revision.test(commit?.sha ?? '') || !Array.isArray(commit.parents)
          || commit.parents.length < 1 || commit.parents.some(parent => !revision.test(parent?.sha ?? '')))) {
        refuse('consumer_composition_graph_unmeasured');
      }
      commits.push(...value.commits);
    }
    if (new Set(commits.map(commit => commit.sha)).size !== commits.length
      || commits.length > 0 && !commits.some(commit => commit.sha === head)) refuse('consumer_composition_graph_unmeasured');
    return commits;
  };
  const currentDefault = await one(`/commits/${encodeURIComponent(repo.default_branch)}`);
  if (!revision.test(currentDefault?.sha ?? '')) refuse('consumer_source_unmeasured');
  await comparison(manifest.base_sha, currentDefault.sha, true);
  const compositionCommits = await comparison(manifest.base_sha, selected.headSha, true);
  const compositionCommitIds = new Set(compositionCommits.map(commit => commit.sha));
  const componentCommits = new Set();
  for (const component of components) {
    await comparison(component.head_sha, selected.headSha, true);
    for (const commit of await comparison(manifest.base_sha, component.head_sha)) {
      if (!compositionCommitIds.has(commit.sha)) refuse('consumer_composition_component_missing');
      componentCommits.add(commit.sha);
    }
  }
  const extra = compositionCommits.filter(commit => !componentCommits.has(commit.sha) && commit.parents.length === 1);
  if (extra.length !== 1 || extra[0].sha !== selected.headSha) refuse('consumer_composition_extra_commit');
  const manifestCommit = await one(`/commits/${selected.headSha}?per_page=100`);
  if (manifestCommit?.sha !== selected.headSha || manifestCommit.parents?.length !== 1
    || manifestCommit.parents[0].sha !== extra[0].parents[0].sha || !Array.isArray(manifestCommit.files)
    || manifestCommit.files.length !== 1 || manifestCommit.files[0].filename !== consumerCompositionPath
    || !['added', 'modified'].includes(manifestCommit.files[0].status)
    || manifestCommit.files[0].sha !== manifestResponse.sha) refuse('consumer_composition_extra_commit');

  // Refresh mutable PR identities after all immutable graph reads. A moving
  // component cannot be hidden by an earlier manifest or compare response.
  for (let index = 0; index < components.length; index++) {
    const final = await pull(String(components[index].pull_request));
    if (JSON.stringify(final) !== JSON.stringify(snapshots[index])) refuse('consumer_composition_component_moved');
  }
  const finalRepository = await one('');
  if (finalRepository?.full_name !== repository || finalRepository.default_branch !== repo.default_branch) {
    refuse('consumer_composition_source_moved');
  }
  const finalDefault = await one(`/commits/${encodeURIComponent(repo.default_branch)}`);
  if (!revision.test(finalDefault?.sha ?? '')) refuse('consumer_source_unmeasured');
  if (finalDefault.sha !== currentDefault.sha) await comparison(manifest.base_sha, finalDefault.sha, true);
  if (JSON.stringify(await pull(pullNumber, true)) !== JSON.stringify(selected)) refuse('consumer_composition_source_moved');
  return Object.freeze({headSha: selected.headSha, branch: selected.branch,
    composition: consumerCompositionIdentity({schema: 'harn.consumer_composition_identity.v1', pull: pullNumber,
      requiredPulls, manifestSha256: createHash('sha256').update('harn.consumer_composition_manifest.v1\0').update(bytes).digest('hex')})});
}
