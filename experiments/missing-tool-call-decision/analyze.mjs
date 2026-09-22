import fs from 'node:fs';
import path from 'node:path';
const root = process.argv[2];
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'manifest.json'), 'utf8'));
const corpus = fs.readFileSync(path.join(root, 'corpus.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
const items = fs.readdirSync(root).flatMap(name => {
  const file = path.join(root, name, 'result.json');
  return fs.existsSync(file) ? [JSON.parse(fs.readFileSync(file, 'utf8'))] : [];
});
const expectedRows = manifest.repetitions * manifest.arms.length * corpus.length;
if (items.some(item => !Number.isFinite(item.cost_usd))) throw Error('study contains unmeasured cost');
if (items.length !== expectedRows) throw Error(`incomplete study: ${items.length}/${expectedRows}`);
const keys = new Set(items.map(x => `${x.repeat}:${x.row_id}:${x.arm}`));
for (let repeat = 0; repeat < manifest.repetitions; repeat++) for (const row of corpus) for (const arm of manifest.arms) {
  if (!keys.has(`${repeat}:${row.row_id}:${arm.id}`)) throw Error('missing study cell');
}
const mean = xs => xs.reduce((a,b) => a+b, 0) / xs.length;
const quantile = (xs, p) => { const sorted = [...xs].sort((a,b) => a-b), at = (sorted.length - 1) * p, low = Math.floor(at); return sorted[low] + (sorted[Math.ceil(at)] - sorted[low]) * (at - low); };
const rowIds = [...new Set(items.map(x => x.row_id))].sort();
const arms = [...new Set(items.map(x => x.arm))].sort();
const observation = item => item.observed.observations[0];
const recovery = item => item.observed.recovery_nudges.length > 0;
const correct = item => recovery(item) === item.expected.intended && (!item.expected.intended || observation(item).verdict.tool_name === item.expected.tool);
let seed = 8543;
const random = () => ((seed = Math.imul(seed, 1664525) + 1013904223 >>> 0) / 4294967296);
function paired(a, b, metric) {
  const values = rowIds.map(id => mean(items.filter(x => x.row_id === id && x.arm === a).map(metric)) - mean(items.filter(x => x.row_id === id && x.arm === b).map(metric)));
  const draws = Array.from({length: 10000}, () => mean(values.map(() => values[Math.floor(random() * values.length)])));
  return {difference: mean(values), paired_row_bootstrap_95: [quantile(draws, .025), quantile(draws, .975)], independent_row_clusters: values.length};
}
const calibration = [];
const report = {design: 'Fixed curated 12-row corpus, five repeated trials. Descriptive fidelity only; bootstrap clusters rows, not repeats. Primary assistant/tool execution replayed, classifier paid.', completed: items.length, arms: {}};
for (const arm of arms) {
  const subset = items.filter(x => x.arm === arm);
  const cache = [];
  for (const item of subset) {
    const obs = observation(item), verdict = obs.verdict;
    const answer = verdict.decision?.value?.intended;
    const legacy = verdict.typed_checkpoint?.data;
    const legacyAnswered = legacy && ['tool_call_intended','no_tool_call_intended'].includes(legacy.action);
    const predicted = answer ? String(answer.verdict) : legacyAnswered ? String(legacy.action === 'tool_call_intended') : '';
    calibration.push({question_id: 'intended', backend: arm, expected: String(item.expected.intended), predicted, confidence: answer?.confidence ?? legacy?.confidence ?? 0, abstained: !answer && !legacyAnswered, cost: item.cost_usd, latency_ms: obs.elapsed_ms});
    const tool = verdict.decision?.value?.tool?.choice ?? legacy?.tool_name ?? '';
    if (item.expected.intended) calibration.push({question_id: 'tool', backend: arm, expected: item.expected.tool, predicted: tool, confidence: verdict.decision?.value?.tool?.confidence ?? legacy?.confidence ?? 0, abstained: !tool});
    const usage = verdict.typed_checkpoint?.usage ?? item.receipts[0]?.usage;
    cache.push(usage && typeof usage.cache_read_tokens === 'number' ? usage.cache_read_tokens : null);
  }
  report.arms[arm] = {rows: subset.length, correct_recovery_and_tool: subset.filter(correct).length, false_recovery: subset.filter(x => recovery(x) && !x.expected.intended).length, missed_recovery: subset.filter(x => !recovery(x) && x.expected.intended).length, ambiguous: subset.filter(x => observation(x).verdict.action === 'ambiguous').length, failures: subset.filter(x => observation(x).verdict.error).length, completed_runs: subset.filter(x => x.observed.status === 'done').length, classifier_latency_ms: {median: quantile(subset.map(x => observation(x).elapsed_ms), .5), max: Math.max(...subset.map(x => observation(x).elapsed_ms))}, run_latency_ms_median: quantile(subset.map(x => x.observed.elapsed_ms), .5), cost_usd: subset.reduce((n,x) => n+x.cost_usd,0), cache: {reported_rows: cache.filter(x => x !== null).length, unavailable_rows: cache.filter(x => x === null).length, positive_rows: cache.filter(x => x > 0).length}, by_language: Object.fromEntries(['en','es','fr'].map(lang => {const xs = subset.filter(x => x.language === lang); return [lang,{rows:xs.length,correct:xs.filter(correct).length}]}))};
}
report.paired = Object.fromEntries(['native_decision','structured_decision'].map(arm => [arm + '_minus_legacy', {accuracy: paired(arm,'legacy_structured', x => Number(correct(x))), latency_ms: paired(arm,'legacy_structured', x => observation(x).elapsed_ms), cost_usd: paired(arm,'legacy_structured', x => x.cost_usd)}]));
if (arms.includes('original_structured')) report.paired.revised_structured_minus_original = {accuracy: paired('structured_decision','original_structured', x => Number(correct(x))), latency_ms: paired('structured_decision','original_structured', x => observation(x).elapsed_ms), cost_usd: paired('structured_decision','original_structured', x => x.cost_usd)};
const pairs = items.filter(x => x.arm === 'native_decision').flatMap(native => {
  const structured = items.find(x => x.arm === 'structured_decision' && x.repeat === native.repeat && x.row_id === native.row_id);
  return structured ? [{same_action: observation(native).verdict.action === observation(structured).verdict.action, same_recovery: recovery(native) === recovery(structured), same_positive_tool: !recovery(native) || !recovery(structured) || observation(native).verdict.tool_name === observation(structured).verdict.tool_name}] : [];
});
report.backend_agreement = {paired_rows: pairs.length, same_action: pairs.filter(x => x.same_action).length, same_recovery: pairs.filter(x => x.same_recovery).length, matching_tool_when_both_recover: pairs.filter(x => x.same_positive_tool).length};
report.served_models = Object.fromEntries(arms.map(arm => [arm, [...new Set(items.filter(x => x.arm === arm).flatMap(x => x.receipts.map(r => r.served_model)))]]));
fs.writeFileSync(path.join(root,'analysis.json'), JSON.stringify(report,null,2));
fs.writeFileSync(path.join(root,'calibration-rows.json'), JSON.stringify(calibration));
console.log(JSON.stringify(report,null,2));
