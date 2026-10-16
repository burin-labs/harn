import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-agreement-control-'));
const arms = ['legacy_structured', 'native_decision', 'structured_decision'];
try {
  fs.writeFileSync(path.join(root, 'manifest.json'), JSON.stringify({repetitions: 1, arms: arms.map(id => ({id}))}));
  fs.writeFileSync(path.join(root, 'corpus.jsonl'), JSON.stringify({row_id: 'control', language: 'en'}) + '\n');
  for (const arm of arms) fs.mkdirSync(path.join(root, arm));
  function measure(recover, sameTool, cost = 0, cacheUsage = null) {
    for (const arm of arms) {
      const tool = arm === 'structured_decision' && !sameTool ? 'edit' : 'read';
      const verdict = {action: recover ? 'tool_call_intended' : 'no_tool_call_intended', tool_name: tool,
        decision: {value: {intended: {verdict: recover, confidence: 1}, tool: {choice: tool, confidence: 1}}}};
      if (cacheUsage) verdict.typed_checkpoint = {usage: cacheUsage};
      fs.writeFileSync(path.join(root, arm, 'result.json'), JSON.stringify({
        repeat: 0, row_id: 'control', language: 'en', arm, cost_usd: cost,
        expected: {intended: recover, tool: 'read'}, receipts: [],
        observed: {status: 'done', elapsed_ms: 1, recovery_nudges: recover ? [{}] : [],
          observations: [{elapsed_ms: 1, verdict}]},
      }));
    }
    execFileSync(process.execPath, [new URL('./analyze.mjs', import.meta.url).pathname, root]);
    return JSON.parse(fs.readFileSync(path.join(root, 'analysis.json'))).backend_agreement;
  }
  const empty = measure(false, true);
  assert.equal(empty.paired_rows, 1);
  assert.equal(empty.both_recover_pairs, 0);
  assert.equal(empty.matching_tool_when_both_recover, 0);
  const matching = measure(true, true);
  assert.equal(matching.both_recover_pairs, 1);
  assert.equal(matching.matching_tool_when_both_recover, 1);
  const mismatch = measure(true, false);
  assert.equal(mismatch.both_recover_pairs, 1);
  assert.equal(mismatch.matching_tool_when_both_recover, 0);
  measure(false, true, null);
  const unavailable = JSON.parse(fs.readFileSync(path.join(root, 'analysis.json')));
  for (const arm of arms) {
    assert.equal(unavailable.arms[arm].cost_usd, null);
    assert.equal(unavailable.cost_accounting[arm].measured_rows, 0);
    assert.equal(unavailable.cost_accounting[arm].unavailable_rows, 1);
    assert.equal(unavailable.arms[arm].cache.unavailable_rows, 1);
  }
  assert.equal(unavailable.paired.native_decision_minus_legacy.cost_usd.difference, null);
  for (const tokens of [5, 0]) {
    measure(false, true, 0, {cache_read_tokens: tokens, cache_supported: true, cache_accounting_declared: true});
    const measured = JSON.parse(fs.readFileSync(path.join(root, 'analysis.json')));
    for (const arm of arms) {
      assert.equal(measured.arms[arm].cache.reported_rows, 1);
      assert.equal(measured.arms[arm].cache.unavailable_rows, 0);
      assert.equal(measured.arms[arm].cache.positive_rows, Number(tokens > 0));
    }
  }
  console.log(JSON.stringify({zero_eligible: empty, matching, mismatch}));
} finally {
  fs.rmSync(root, {recursive: true, force: true});
}
