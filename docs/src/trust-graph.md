# Trust graph

Harn's trust graph is the runtime-owned event stream for autonomy decisions.
Every trigger dispatch now appends a hash-chained OpenTrustGraph `TrustRecord`
to `trust_graph` plus the per-agent topic `trust_graph.<agent_id>`. It also
mirrors a compact `TrustGraphRecord` projection to `trust_graph.records` for
runtime policy and application queries. Harn still reads and mirrors the older
`trust.graph` topic names for compatibility. The same stream also carries
explicit promotion and demotion events recorded by `harn trust promote` and
`harn trust demote`.

## Record model

Each record carries:

- `schema` (`"opentrustgraph/v0.1"`; v0 records still parse for one patch
  release window — see [OpenTrustGraph CONFORMANCE.md §5][otg-versioning])
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
- `metadata` (v0.1 reserves `effects_grant`, `effects_used`,
  `parent_record_id`, `actor_chain`, and `actor_chain_alert` so chain
  validators can prove child effect inheritance, actor-chain parentage, and
  actor-chain policy alerts)

[otg-versioning]: https://github.com/burin-labs/harn/blob/main/opentrustgraph-spec/CONFORMANCE.md#5-versioning

The `trust_graph.records` projection carries the runtime query shape:

- `actor_id`
- `action`
- `approver`
- `outcome`
- `evidence_refs`
- `trace_id`
- `timestamp`
- `autonomy_tier_at_time`

See [`spec/opentrustgraph.md`](../../spec/opentrustgraph.md) for the normative
record model, chain export model, and sample event stream. The small public
artifact that cloud receipts, IDE-host supervision UI planning, and external
verifiers should link to is
[`opentrustgraph-spec/`](../../opentrustgraph-spec/). It contains the JSON
Schema files and conformance fixtures used by Harn's runtime tests.

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
The dispatcher enforces the tier by translating it into a capability policy
before handler code runs: `shadow` denies side effects, `suggest` and
`act_with_approval` are read-only for direct builtins, and `act_auto` keeps the
normal network-capable ceiling. Shadow and suggest rejections also emit proposal
metadata for operators.

## CLI

Query the trust graph:

```bash
harn trust query --agent github-triage-bot
harn trust query --agent github-triage-bot --outcome denied --json
harn trust query --summary
harn trust-graph verify-chain
harn trust-graph export --output chain.json
```

`harn trust-graph export` writes the canonical
[`opentrustgraph-chain/v0`](spec/open-trust-graph/v0.md) envelope. With no
`--output`, it prints the JSON to stdout. Pipe it through any conformant
verifier — e.g. the reference Python verifier:

```bash
harn trust-graph export | python3 opentrustgraph-spec/examples/python/verify_chain.py
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

Import `std/trust` for the focused TrustGraph API:

- `query(filters)` returns `TrustGraphRecord` rows. Filters accept `actor`,
  `actor_id`, `agent`, `action`, `outcome`, `since`, `until`,
  `autonomy_tier_at_time`, `tier`, and `limit`.
- `record(decision)` appends a trust decision and returns its `TrustEntryId`.
  Decisions accept `actor_id` (or `agent`), `action`, `approver`, `outcome`,
  `trace_id`, `autonomy_tier_at_time` (or `autonomy_tier`/`tier`),
  `evidence_refs`, `cost_usd`, and `metadata`.
- `score(actor_id, action?)` returns a `TrustScore`.
- `policy_for(actor_id)` returns the capability policy derived from trust
  history.
- `verify_chain()` verifies the underlying OpenTrustGraph hash chain.

The native namespace exposes the same focused query as `trust.query(...)`,
matching the API shape `harn::trust::query({actor: ..., action: ...})` in host
integrations.

Import `std/corrections` for replay-for-teaching records:

- `record(correction: CorrectionInput)` appends a `CorrectionRecord` with
  typed `CorrectionDecision` values for `from_decision` and `to_decision`,
  `reason`, `applied_by`, and `scope` (`this_run`, `this_persona`, or `all`),
  plus optional `actor_id`, `action`, `trace_id`, `step`,
  `list<CorrectionEvidenceRef>`, and `metadata`.
- `query(filters: CorrectionQueryFilters)` returns corrections by `actor`,
  `actor_id`, `agent`, `action`, `scope`, `since`, `until`, and `limit`.

`this_persona` and `all` corrections are policy inputs. When a human keeps
correcting an actor's decision path, `policy_for(actor_id)` caps that actor's
derived side-effect level at `read_only` while the matching correction records
remain applicable.

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
import "std/trust"

let records: list<TrustGraphRecord> = query({
  actor: "github-triage-bot",
  action: "merge_pr",
  outcome: "success",
  since: "2026-04-01T00:00:00Z",
})
```

The legacy trigger API remains available:

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
reading. A supervision UI can project that response into the
`opentrustgraph-chain/v0` envelope by using the verification report's topic,
total, root hash, and verified flag as `chain` metadata and the returned records
as `records`.
