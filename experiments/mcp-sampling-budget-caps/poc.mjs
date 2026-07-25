#!/usr/bin/env node
// Prototype for the MCP per-call sampling budget-caps RFC.
//
// See ../../docs/src/protocol-contributions/mcp-sampling-budget-caps.md
// Upstream discussion: modelcontextprotocol/modelcontextprotocol#2736
//
// Run: node poc.mjs
//
// No dependencies, no network, no API keys. A stub model prices tokens
// from a fixed rate card so the numbers below are reproducible.
//
// What this demonstrates:
//   1. A server may declare advisory budget intent; the host's own limit wins.
//   2. A pre-flight refusal returns stopReason "budget_exceeded", no tokens spent.
//   3. A mid-generation stop returns "budget_exhausted" with partial content kept.
//   4. A transport failure stays a JSON-RPC error and is NOT a budget decision.
//
// Case 4 is the one that justifies the whole proposal: today all four of
// these arrive at the server as an indistinguishable error.

const RATE_CARD = {
  'example-model-v2': { inPerMTok: 3.0, outPerMTok: 15.0, version: '2026-07-01' },
};

// Decimal-string money. Cost caps are compared as integer micro-units so the
// prototype never leans on float equality for a policy decision.
const MICRO = 1_000_000;
const toMicros = (amount) => Math.round(Number.parseFloat(amount) * MICRO);
const fromMicros = (micros) => (micros / MICRO).toFixed(4);

function meterBasis(model) {
  const r = RATE_CARD[model];
  return `${model}@${r.version}:in=${r.inPerMTok.toFixed(2)}/Mtok,out=${r.outPerMTok.toFixed(2)}/Mtok`;
}

function priceMicros(model, inputTokens, outputTokens) {
  const r = RATE_CARD[model];
  const dollars =
    (inputTokens / 1_000_000) * r.inPerMTok + (outputTokens / 1_000_000) * r.outPerMTok;
  return Math.round(dollars * MICRO);
}

// ---------------------------------------------------------------------------
// Stub model. Emits one chunk per call to `next()` so the host can stop it
// partway and observe that partial content survives.
// ---------------------------------------------------------------------------

function stubModel({ model, inputTokens, chunks }) {
  let emitted = 0;
  return {
    model,
    inputTokens,
    next() {
      if (emitted >= chunks.length) return null;
      const chunk = chunks[emitted++];
      return { text: chunk.text, outputTokens: chunk.outputTokens };
    },
  };
}

// ---------------------------------------------------------------------------
// Host. Owns the policy limit. Enforces before and during the call.
// ---------------------------------------------------------------------------

class Host {
  constructor({ policyLimit, transport }) {
    this.policyLimit = policyLimit; // decimal string, USD
    this.transport = transport;
  }

  // Handles a sampling/createMessage request from a server.
  handleCreateMessage(params) {
    if (this.transport === 'fail') {
      // Transport problems are JSON-RPC errors and carry no budget decision.
      // A server MUST be able to tell this apart from a policy refusal.
      const err = new Error('upstream connection reset');
      err.jsonRpcCode = -32001;
      throw err;
    }

    const model = 'example-model-v2';
    const limitMicros = toMicros(this.policyLimit);

    const requested = params.budget?.intent?.maxCost;
    const onExceeded = params.budget?.onExceeded ?? 'reject';

    const inputTokens = params._stubInputTokens;
    const plannedOutput = params.maxTokens;

    // --- Pre-flight estimate -------------------------------------------------
    const estimateMicros = priceMicros(model, inputTokens, plannedOutput);
    const decision = {
      estimatedCost: { amount: fromMicros(estimateMicros), currency: 'USD' },
      limitApplied: { amount: fromMicros(limitMicros), currency: 'USD' },
      limitSource: 'host_policy',
      meterBasis: meterBasis(model),
      estimatedTokens: { input: inputTokens, output: plannedOutput },
    };

    // The server's declared intent is advisory. Recorded, never authoritative:
    // if the server asked for more than policy allows, policy still wins.
    if (requested) {
      decision.requestedCost = requested;
      decision.requestHonored = toMicros(requested.amount) <= limitMicros;
    }

    if (estimateMicros > limitMicros && onExceeded === 'reject') {
      return {
        role: 'assistant',
        content: { type: 'text', text: '' },
        model,
        stopReason: 'budget_exceeded',
        budget: { decision },
      };
    }

    // --- Run, metering as we go ---------------------------------------------
    const call = stubModel({
      model,
      inputTokens,
      chunks: params._stubChunks,
    });

    let text = '';
    let outputTokens = 0;
    let spentMicros = priceMicros(model, inputTokens, 0);
    let stopReason = 'endTurn';

    for (;;) {
      const chunk = call.next();
      if (chunk === null) break;

      const chunkMicros = priceMicros(model, 0, chunk.outputTokens);
      if (spentMicros + chunkMicros > limitMicros) {
        // Stop before the chunk that would cross the cap. Everything already
        // generated stays valid and is returned to the server.
        stopReason = 'budget_exhausted';
        break;
      }

      text += chunk.text;
      outputTokens += chunk.outputTokens;
      spentMicros += chunkMicros;
    }

    decision.actualCost = { amount: fromMicros(spentMicros), currency: 'USD' };
    decision.actualTokens = { input: inputTokens, output: outputTokens };

    return {
      role: 'assistant',
      content: { type: 'text', text },
      model,
      stopReason,
      budget: { decision },
    };
  }
}

// ---------------------------------------------------------------------------
// Server side. Only reacts to what the protocol tells it.
// ---------------------------------------------------------------------------

function serverDecideNextAction(result) {
  switch (result.stopReason) {
    case 'budget_exceeded': {
      // Actionable because the decision basis is present: compute how much
      // smaller the request must be instead of retrying blindly.
      const est = toMicros(result.budget.decision.estimatedCost.amount);
      const lim = toMicros(result.budget.decision.limitApplied.amount);
      const shrinkTo = Math.floor((lim / est) * 100);
      return `shrink request to ~${shrinkTo}% of input and retry once`;
    }
    case 'budget_exhausted':
      return 'use the partial content; do not retry (cap will bind again)';
    case 'endTurn':
      return 'use the completion';
    default:
      return `unhandled stopReason: ${result.stopReason}`;
  }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

const CHUNKS = [
  { text: 'The cap ', outputTokens: 400 },
  { text: 'binds partway ', outputTokens: 400 },
  { text: 'through this sentence ', outputTokens: 400 },
  { text: 'and never reaches the end.', outputTokens: 400 },
];

const scenarios = [
  {
    name: '1. under budget, server declared intent within policy',
    policyLimit: '0.5000',
    params: {
      maxTokens: 1600,
      budget: { intent: { maxCost: { amount: '0.2000', currency: 'USD' } }, onExceeded: 'reject' },
      _stubInputTokens: 1000,
      _stubChunks: CHUNKS,
    },
    expect: { stopReason: 'endTurn', requestHonored: true },
  },
  {
    name: '2. pre-flight refusal, estimate over host policy limit',
    policyLimit: '0.0500',
    params: {
      maxTokens: 2048,
      budget: { intent: { maxCost: { amount: '2.0000', currency: 'USD' } }, onExceeded: 'reject' },
      _stubInputTokens: 24000,
      _stubChunks: CHUNKS,
    },
    // Server asked for $2.00; host policy is $0.05. Intent does not win.
    expect: { stopReason: 'budget_exceeded', requestHonored: false, emptyContent: true },
  },
  {
    name: '3. mid-generation stop, server asked to be truncated',
    policyLimit: '0.0140',
    params: {
      maxTokens: 1600,
      budget: { onExceeded: 'truncate' },
      _stubInputTokens: 1000,
      _stubChunks: CHUNKS,
    },
    expect: { stopReason: 'budget_exhausted', partialContent: true },
  },
  {
    name: '4. transport failure is not a budget decision',
    policyLimit: '0.5000',
    transport: 'fail',
    params: { maxTokens: 1600, _stubInputTokens: 1000, _stubChunks: CHUNKS },
    expect: { jsonRpcError: -32001 },
  },
];

let failures = 0;
const check = (label, actual, wanted) => {
  const ok = actual === wanted;
  if (!ok) failures++;
  console.log(`     ${ok ? 'ok  ' : 'FAIL'} ${label}: ${JSON.stringify(actual)}${ok ? '' : ` (wanted ${JSON.stringify(wanted)})`}`);
};

for (const s of scenarios) {
  console.log(`\n${s.name}`);
  console.log(`   host policy limit: $${s.policyLimit}`);

  const host = new Host({ policyLimit: s.policyLimit, transport: s.transport });

  let result = null;
  let error = null;
  try {
    result = host.handleCreateMessage(s.params);
  } catch (e) {
    error = e;
  }

  if (s.expect.jsonRpcError !== undefined) {
    check('threw a JSON-RPC error', error?.jsonRpcCode, s.expect.jsonRpcError);
    check('no budget decision attached', result, null);
    console.log(`     server sees: transport error, retry is reasonable`);
    continue;
  }

  const d = result.budget.decision;
  console.log(`   estimate: $${d.estimatedCost.amount}  limit: $${d.limitApplied.amount}  basis: ${d.meterBasis}`);
  check('stopReason', result.stopReason, s.expect.stopReason);

  if (s.expect.requestHonored !== undefined) {
    check('server intent honored', d.requestHonored, s.expect.requestHonored);
  }
  if (s.expect.emptyContent) {
    check('content empty (no tokens spent)', result.content.text, '');
    check('no actual cost recorded', d.actualCost, undefined);
  }
  if (s.expect.partialContent) {
    check('partial content preserved', result.content.text.length > 0, true);
    check(
      'actual cost within limit',
      toMicros(d.actualCost.amount) <= toMicros(d.limitApplied.amount),
      true,
    );
  }
  console.log(`     server next action: ${serverDecideNextAction(result)}`);
}

console.log(
  `\n${failures === 0 ? 'all scenarios passed' : `${failures} assertion(s) FAILED`}`,
);
process.exit(failures === 0 ? 0 : 1);
