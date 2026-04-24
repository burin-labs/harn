# Team Learning and Context Packs

Harn can turn repeated team friction into reviewable context packs or promoted workflows. The loop is:

1. A workflow or host shim records a structured friction event.
2. Repeated events become evidence for a candidate context pack suggestion.
3. A human reviews the suggested manifest, privacy notes, and estimated savings.
4. Future runs load deterministic context first and ask fewer repeated questions.

This is different from generic memory. A context pack is structured, reviewable,
capability-scoped, and measurable. It points at explicit queries, docs, tools,
secrets, refresh policy, output slots, and fallback instructions instead of
storing raw conversation history.

## Friction Events

Use `friction_record(payload, options?)` to record repeated pain from Harn workflows
or host integrations. With no configured recorder, the event is stored in the
process-local friction buffer and the workflow keeps running. Set `enabled: false`
for a deliberate no-op, or pass `log_path` / `HARN_FRICTION_LOG` to append JSONL.

```harn
friction_record({
  kind: "repeated_query",
  source: "incident-triage",
  actor: "sre",
  run_id: "run_checkout_184",
  tool: "splunk",
  provider: "splunk",
  redacted_summary: "Checkout incidents repeatedly need the same error search",
  estimated_time_ms: 300000,
  estimated_cost_usd: 0.12,
  recurrence_hints: ["checkout incident queries"],
  trace_id: "trace_01H...",
  metadata: {
    query: "index=checkout service=api error",
    capability: "splunk.search",
    secret_ref: "SPLUNK_READ_TOKEN",
    output_slot: "splunk_errors",
  },
})
```

Supported event kinds are `repeated_query`, `repeated_clarification`, `approval_stall`,
`missing_context`, `manual_handoff`, `tool_gap`, `failed_assumption`,
`expensive_model_used_for_deterministic_step`, and `human_hypothesis`.

Events intentionally keep `redacted_summary` as the user-facing text field. Raw
prompts, raw content, and secret-looking metadata are dropped or redacted by the
normalizer.

## Context Pack Manifests

`context_pack_manifest(payload)` validates a manifest shape. `context_pack_manifest_parse(src)` accepts TOML or JSON.

```toml
version = 1
id = "checkout_incident_context"
name = "Checkout incident context"
description = "Gather deterministic incident triage context before an agent starts."
owner = "sre"
fallback_instructions = "Ask one scoped question if the deterministic context is insufficient."
capabilities = ["splunk.search", "honeycomb.board.read"]

[[triggers]]
kind = "repeated_query"
source = "incident-triage"
match_hint = "checkout incident queries"

[[inputs]]
name = "incident_id"
required = true

[[included_queries]]
id = "splunk_errors"
provider = "splunk"
query = "index=checkout service=api error"
output_slot = "splunk_errors"

[[included_docs]]
id = "runbook"
title = "Checkout incident runbook"
url = "https://notion.example/runbooks/checkout"

[[included_tools]]
name = "honeycomb_board"
capability = "honeycomb.board.read"
purpose = "Open the checkout latency board."
deterministic = true

[refresh_policy]
mode = "on_demand"
stale_after = "24h"

[[secrets]]
name = "SPLUNK_READ_TOKEN"
capability = "splunk.search"
required = true

[[output_slots]]
name = "splunk_errors"
artifact_kind = "context"
```

Secrets are references to host-managed capabilities, not raw token values.

## Suggestions and Evals

`context_pack_suggestions(events?, options?)` groups repeated friction and emits
candidate suggestion artifacts with evidence, example summaries, estimated savings,
risk/privacy notes, and a draft manifest. `friction_eval_fixture(fixture)` is the
stdlib smoke path for fixture-driven checks.

Eval packs can also run repeated-friction fixtures:

```toml
version = 1
id = "team-learning"

[[fixtures]]
id = "incident-friction"
kind = "friction-events"
path = "fixtures/incident-friction.json"

[[cases]]
id = "incident-context-pack"
friction_events = "incident-friction"
rubrics = ["context-pack"]

[[rubrics]]
id = "context-pack"
kind = "friction"

[[rubrics.assertions]]
kind = "context-pack-suggestion"
contains = "incident"
expected = {
  min_suggestions = 1,
  recommended_artifact = "context_pack",
  required_capability = "splunk.search",
}
```

The fixture should contain either a JSON array of friction events or `{ "events": [...] }`.
Local evaluation stays deterministic and does not call an LLM judge.
