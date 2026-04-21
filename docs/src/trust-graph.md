# Trust graph

Harn's trust graph is the runtime-owned event stream for autonomy decisions.
Every trigger dispatch now appends a `TrustRecord` to `trust_graph` plus the
per-agent topic `trust_graph.<agent_id>`. Harn still reads and mirrors the older
`trust.graph` topic names for compatibility. The same stream also carries explicit
promotion and demotion events recorded by `harn trust promote` and
`harn trust demote`.

## Record model

Each record carries:

- `schema` (`"opentrustgraph/v0"`)
- `record_id`
- `agent`
- `action`
- `approver`
- `outcome`
- `trace_id`
- `autonomy_tier`
- `timestamp`
- `cost_usd`
- `chain_index`
- `previous_hash`
- `entry_hash`
- `metadata`

See [`spec/opentrustgraph.md`](../../spec/opentrustgraph.md) for the normative
schema, JSON Schema, and sample event stream.

## Autonomy tiers

Trigger manifests and dynamic trigger registrations can set
`autonomy_tier` to one of:

- `shadow`: run in dry-run mode. Mutating builtins are rejected and the runtime
  emits proposal metadata instead of performing the mutation.
- `suggest`: emit a proposal and wait for an approval path before mutation.
- `act_with_approval`: handlers execute, but direct mutating actions are still
  gated behind approval-aware surfaces.
- `act_auto`: mutating actions run freely; dispatches are still logged.

At runtime, handlers can inspect the effective tier with
`handler_context().autonomy_tier`. Harn resolves that effective tier from the
manifest default plus the latest trust-graph control record for the agent.

## CLI

Query the trust graph:

```bash
harn trust query --agent github-triage-bot
harn trust query --agent github-triage-bot --outcome denied --json
harn trust query --summary
harn trust-graph verify-chain
```

Promote or demote an agent:

```bash
harn trust promote github-triage-bot --to act-auto
harn trust demote github-triage-bot --to shadow --reason "unexpected prod mutation"
```

`--summary` aggregates records per agent and reports:

- success rate
- mean cost
- tier distribution
- outcome distribution

## Script APIs

Import `std/triggers` and use:

- `handler_context()` to inspect the current dispatch context, including
  `agent`, `action`, `trace_id`, and `autonomy_tier`
- `trust_record(agent, action, approver, outcome, tier)` to append a manual
  trust record and return the full finalized record
- `trust_graph_record(decision)` to append a decision dict and return its
  `TrustEntryId`
- `trust_query(filters)` to query historical records from Harn code, including
  server-side `limit` and `grouped_by_trace` options
- `trust_graph_query(agent, action)` to return a `TrustScore` summary for
  handler-side policy decisions
- `trust_graph_policy_for(agent)` to return a capability policy derived from
  the agent's effective tier and recent outcomes
- `trust_graph_verify_chain()` to verify the local hash chain

Example:

```harn
import "std/triggers"

let records = trust_query({
  agent: "github-triage-bot",
  outcome: "success",
  tier: "act_auto",
  limit: 100,
})
```

Grouped queries return trace buckets:

```harn
import "std/triggers"

let grouped = trust_query({
  since: "2026-04-19T18:00:00Z",
  limit: 500,
  grouped_by_trace: true,
})
```

`self_review(...)` also writes trust records with action `pr.self_review`.
Its metadata currently includes:

- `rubric` and `rubric_preset`
- `requested_rounds` and `completed_rounds`
- `finding_count` and `blocking_finding_count`
- `secret_scan_finding_count`
- `finding_categories`
- `summary`
- `diff_bytes` and `diff_sha256`

## Portal API

The portal exposes `GET /api/trust-graph` for external UIs. Query parameters
mirror the CLI filters: `agent`, `action`, `limit`, and `grouped_by_trace`.
The response includes flat records, optional trace groups, per-agent summary
rows, the current chain verification report, and the topic names the portal is
reading.
