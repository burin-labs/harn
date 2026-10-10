import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';

const [binary, workspace, root, repetitionsArg = '5', baselineRef = 'origin/main', corpusName = 'cutover-2026-10-10.jsonl'] = process.argv.slice(2);
const repetitions = Number(repetitionsArg);
if (!binary || !workspace || !root || !Number.isInteger(repetitions) || repetitions < 1) {
  throw Error('usage: node run-cutover.mjs BINARY WORKSPACE OUTPUT [REPETITIONS] [BASELINE_REF] [CORPUS_FILENAME]');
}
const hash = value => crypto.createHash('sha256').update(value).digest('hex');
const git = args => {
  const result = spawnSync('git', args, {cwd: workspace, encoding: 'utf8'});
  if (result.status !== 0) throw Error(`baseline source read failed: ${result.stderr}`);
  return result.stdout;
};
const baselineSha = git(['rev-parse', baselineRef]).trim();
if (!/^[0-9a-f]{40}$/.test(baselineSha)) throw Error('baseline must resolve to one commit');
const corpusFile = path.join(workspace, 'experiments/missing-tool-call-decision', corpusName);
const corpusText = fs.readFileSync(corpusFile, 'utf8');
const corpus = corpusText.trim().split('\n').map(JSON.parse);
if (!corpus.length || new Set(corpus.map(row => row.row_id)).size !== corpus.length ||
    !corpus.every(row => typeof row.row_id === 'string' && row.row_id.length > 0 &&
      ['en', 'es', 'fr'].includes(row.language) && typeof row.assistant_text === 'string' &&
      Buffer.byteLength(row.assistant_text) <= 4096 && typeof row.intended === 'boolean' &&
      (row.intended ? ['read', 'edit', 'shell'].includes(row.tool) : row.tool === ''))) {
  throw Error('corpus must contain unique, bounded, labeled assistant texts');
}
const env = {...process.env};
const allowedKeys = ['OPENAI_API_KEY', 'OPENROUTER_API_KEY'];
for (const line of fs.readFileSync(path.join(os.homedir(), 'gate-clone/.env'), 'utf8').split('\n')) {
  const match = line.match(/^([A-Z0-9_]+)=(.*)$/);
  if (match && allowedKeys.includes(match[1])) env[match[1]] = match[2].replace(/^(['"])(.*)\1$/, '$2');
}
fs.mkdirSync(root, {recursive: true});
const legacySource = git(['show', `${baselineSha}:crates/harn-stdlib/src/stdlib/llm/missing_tool_call.harn`]);
const legacyPrompt = git(['show', `${baselineSha}:crates/harn-stdlib/src/stdlib/llm/prompts/missing_tool_call_classifier.harn.prompt`]);
const oldPromptPath = 'std/llm/prompts/missing_tool_call_classifier.harn.prompt';
if (legacySource.split(oldPromptPath).length !== 2) throw Error('baseline prompt binding changed');
fs.writeFileSync(path.join(root, 'legacy_classifier.harn.prompt'), legacyPrompt);
fs.writeFileSync(path.join(root, 'legacy_missing_tool_call.harn'), legacySource.replace(oldPromptPath, path.join(root, 'legacy_classifier.harn.prompt')));
const replayFile = path.join(workspace, 'experiments/missing-tool-call-decision/replay.harn');
const replay = fs.readFileSync(replayFile, 'utf8');
const legacyImport = 'import { missing_tool_call_classifier } from "std/llm/missing_tool_call"';
if (replay.split(legacyImport).length !== 2) throw Error('replay classifier binding changed');
fs.writeFileSync(path.join(root, 'legacy_replay.harn'), replay.replace(legacyImport, 'import { missing_tool_call_classifier } from "./legacy_missing_tool_call"'));
const candidateSourceFile = path.join(workspace, 'crates/harn-stdlib/src/stdlib/llm/missing_tool_call.harn');
fs.writeFileSync(path.join(root, 'candidate_replay.harn'), replay.replace(legacyImport,
  `import { missing_tool_call_classifier } from "${candidateSourceFile.slice(0, -5)}"`));
const arms = [
  {id: 'legacy_structured', script: path.join(root, 'legacy_replay.harn'), config: {provider: 'openai', model: 'gpt-5.4-nano', effort: 'none'}},
  {id: 'structured_decision', script: path.join(root, 'candidate_replay.harn'), config: {provider: 'openai', model: 'gpt-5.4-nano', effort: 'none'}},
  {id: 'native_decision', script: path.join(root, 'candidate_replay.harn'), config: {evaluation: {backend: 'native_decision', provider: 'openrouter', model: 'openrouter/typesafe/jev-1.13', threshold: 0.0, evaluation_cost_limit: 0.05, run_cost_limit: 0.5}}},
];
const manifest = {
  candidate_sha: git(['rev-parse', 'HEAD']).trim(),
  classifier_source_binding: 'explicit owning source modules for both baseline and candidate',
  candidate_classifier_sha256: hash(fs.readFileSync(candidateSourceFile)),
  candidate_intent_sha256: hash(fs.readFileSync(path.join(workspace, 'crates/harn-stdlib/src/stdlib/llm/prompts/missing_tool_call_intent.harn.prompt'))),
  candidate_target_sha256: hash(fs.readFileSync(path.join(workspace, 'crates/harn-stdlib/src/stdlib/llm/prompts/missing_tool_call_target.harn.prompt'))),
  baseline_sha: baselineSha, baseline_classifier_sha256: hash(legacySource), baseline_prompt_sha256: hash(legacyPrompt),
  corpus_sha256: hash(corpusText), replay_sha256: hash(replay), binary_sha256: hash(fs.readFileSync(binary)),
  repetitions, rows: corpus.length, maximum_spend_usd: 1,
  arms: arms.map(({id, config}) => ({id, config})),
};
const manifestPath = path.join(root, 'manifest.json');
if (fs.existsSync(manifestPath) && fs.readFileSync(manifestPath, 'utf8') !== JSON.stringify(manifest, null, 2)) throw Error('study identity changed');
fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2));
fs.writeFileSync(path.join(root, 'corpus.jsonl'), corpusText);
let totalCost = 0, spendBound = 0, unmeasuredCost = 0, completed = 0;
for (let repeat = 0; repeat < repetitions; repeat++) {
  const rows = [...corpus].sort((a, b) => hash(`${repeat}:${a.row_id}`).localeCompare(hash(`${repeat}:${b.row_id}`)));
  for (let rowIndex = 0; rowIndex < rows.length; rowIndex++) {
    const row = rows[rowIndex];
    const ordered = arms.map((_, i) => arms[(i + repeat + rowIndex) % arms.length]);
    for (const arm of ordered) {
      const dir = path.join(root, `${repeat}-${row.row_id}-${arm.id}`);
      const resultPath = path.join(dir, 'result.json');
      if (fs.existsSync(resultPath)) {
        const cost = JSON.parse(fs.readFileSync(resultPath)).cost_usd;
        totalCost += cost ?? 0;
        spendBound += cost ?? 0.05;
        if (cost === null) unmeasuredCost++;
        completed++;
        continue;
      }
      if (spendBound > 0.95) throw Error('study spend ceiling reached');
      fs.mkdirSync(dir, {recursive: true});
      const configFile = path.join(dir, 'config.json');
      const rowFile = path.join(dir, 'row.json');
      const tapeFile = path.join(dir, 'evaluation.json');
      fs.writeFileSync(configFile, JSON.stringify(arm.config));
      fs.writeFileSync(rowFile, JSON.stringify(row));
      const args = ['run', '--standalone', '--read-only-root', root, '--timeout', '60s', '--emit-summary-json', '--summary-file', path.join(dir, 'summary.json')];
      if (arm.id !== 'legacy_structured') args.push('--evaluation-tape', tapeFile);
      args.push(arm.script, '--', configFile, rowFile);
      const run = spawnSync(binary, args, {cwd: workspace, env, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024});
      fs.writeFileSync(path.join(dir, 'stdout.txt'), run.stdout ?? '');
      fs.writeFileSync(path.join(dir, 'stderr.txt'), run.stderr ?? '');
      if (run.status !== 0) throw Error(`instrument failed: ${arm.id} ${row.row_id}; see ${dir}`);
      const observed = JSON.parse(run.stdout.trim());
      if (observed.observations.length !== 1) throw Error(`classifier count ${observed.observations.length}; see ${dir}`);
      const observation = observed.observations[0];
      if (observed.verdict_events.length !== 1 || observed.status !== 'done') throw Error(`completion failed; see ${dir}`);
      const receipts = arm.id === 'legacy_structured' ? [] : fs.readFileSync(tapeFile, 'utf8').trim().split('\n').map(JSON.parse)
        .filter(record => record.type === 'record').map(record => {
          const payload = record.kind.response;
          const content = payload.text ?? fs.readFileSync(path.join(tapeFile + '.cas', payload.content_hash), 'utf8');
          return JSON.parse(content).receipt;
        });
      if (arm.id !== 'legacy_structured' && (receipts.length !== 1 || receipts[0].physical_attempts !== 1 || !['settled', 'usage_unknown'].includes(receipts[0].accounting_status))) throw Error(`unmeasured evaluator; see ${dir}`);
      if (arm.id === 'legacy_structured' && !(observation.verdict.typed_checkpoint?.usage?.provider_call_count >= 1)) throw Error(`baseline never dispatched; see ${dir}`);
      const cost = observation.cost_usd;
      if (cost !== null && !Number.isFinite(cost)) throw Error(`invalid cost; see ${dir}`);
      if (cost === null ? observed.cost.total_cost !== null : !Number.isFinite(observed.cost.total_cost) || Math.abs(cost - observed.cost.total_cost) > 1e-9) throw Error(`classifier and completing-run accounting disagree; see ${dir}`);
      if (receipts.length && (cost === null ? receipts[0].cost_usd !== null : !Number.isFinite(receipts[0].cost_usd) || Math.abs(cost - receipts[0].cost_usd) > 1e-9)) throw Error(`receipt and session accounting disagree; see ${dir}`);
      const item = {repeat, row_id: row.row_id, language: row.language, arm: arm.id,
        expected: {intended: row.intended, tool: row.tool}, cost_usd: cost, observed, receipts};
      fs.writeFileSync(resultPath, JSON.stringify(item, null, 2));
      totalCost += cost ?? 0;
      // Reserve a deliberately loose allowance for an unpriced request on
      // these fixed small inputs, rather than counting missing usage as free.
      spendBound += cost ?? 0.05;
      if (cost === null) unmeasuredCost++;
      completed++;
      if (completed % 12 === 0) console.log(JSON.stringify({completed, expected: repetitions * corpus.length * arms.length, measured_cost_usd: totalCost, unmeasured_cost_rows: unmeasuredCost, spend_allowance_usd: spendBound}));
    }
  }
}
console.log(JSON.stringify({completed, expected: repetitions * corpus.length * arms.length, measured_cost_usd: totalCost, unmeasured_cost_rows: unmeasuredCost, spend_allowance_usd: spendBound}));
