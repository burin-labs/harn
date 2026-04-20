# Trust graph

Harn's trust graph is the runtime-owned event stream for autonomy decisions.
Every trigger dispatch now appends a `TrustRecord` to `trust.graph` plus the
per-agent topic `trust.graph.<agent_id>`. The same stream also carries explicit
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
  trust record
- `trust_query(filters)` to query historical records from Harn code

Example:

```harn
import "std/triggers"

let records = trust_query({
  agent: "github-triage-bot",
  outcome: "success",
  tier: "act_auto",
})
```
