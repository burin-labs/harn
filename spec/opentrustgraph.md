# OpenTrustGraph v0

`OpenTrustGraph` is a portable event schema for recording autonomy and approval
decisions around agent dispatch. Harn emits these records onto `trust_graph`
plus the per-agent topic `trust_graph.<agent_id>` (`trust.graph` is still read
for compatibility), but the format is designed to
be runtime-neutral so other schedulers and workflow engines can adopt the same
stream shape.

Version marker:

```json
{"schema":"opentrustgraph/v0"}
```

## TrustRecord

Each dispatch or control-plane autonomy change appends one `TrustRecord`.

```json
{
  "schema": "opentrustgraph/v0",
  "record_id": "01966f4c-0f31-7b5d-b44b-f7f8e7e1d384",
  "agent": "github-triage-bot",
  "action": "issue.label",
  "approver": "maintainer-1",
  "outcome": "success",
  "trace_id": "trace_01J...",
  "autonomy_tier": "act_with_approval",
  "timestamp": "2026-04-19T18:42:11Z",
  "cost_usd": 0.0124,
  "chain_index": 7,
  "previous_hash": "sha256:12b6...",
  "entry_hash": "sha256:f00d...",
  "metadata": {
    "provider": "github",
    "binding_version": 3
  }
}
```

Fields:

- `schema`: schema/version discriminator. Current value: `opentrustgraph/v0`.
- `record_id`: globally unique record identifier. UUIDv7 is recommended.
- `agent`: logical agent identifier, handler id, or runtime-owned agent name.
- `action`: action class being evaluated, such as `issue.label`,
  `pr.merge`, or `deploy.prod`.
- `approver`: optional approving actor when an approval gate was satisfied.
- `outcome`: terminal outcome for the dispatch or control change.
- `trace_id`: execution trace id tying the record back to the originating run.
- `autonomy_tier`: autonomy mode in force for this dispatch.
- `timestamp`: RFC3339 UTC timestamp for the record append time.
- `cost_usd`: optional marginal cost attributed to the action.
- `chain_index`: 1-based position in the append-only trust graph chain.
- `previous_hash`: prior record's `entry_hash`, or `null` for the first record.
- `entry_hash`: SHA-256 hash over the canonical record with `entry_hash`
  removed. Harn stores it with the `sha256:` prefix.
- `metadata`: extensible runtime-specific detail bag.

Outcome enum:

- `success`
- `failure`
- `denied`
- `timeout`

Autonomy tier enum:

- `shadow`
- `suggest`
- `act_with_approval`
- `act_auto`

## JSON Schema

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://harnlang.com/schemas/opentrustgraph/v0/trust-record.schema.json",
  "title": "OpenTrustGraph TrustRecord",
  "type": "object",
  "additionalProperties": false,
  "required": [
    "schema",
    "record_id",
    "agent",
    "action",
    "outcome",
    "trace_id",
    "autonomy_tier",
    "timestamp",
    "chain_index",
    "previous_hash",
    "entry_hash",
    "metadata"
  ],
  "properties": {
    "schema": {
      "const": "opentrustgraph/v0"
    },
    "record_id": {
      "type": "string",
      "description": "UUIDv7 recommended."
    },
    "agent": {
      "type": "string",
      "minLength": 1
    },
    "action": {
      "type": "string",
      "minLength": 1
    },
    "approver": {
      "type": ["string", "null"]
    },
    "outcome": {
      "type": "string",
      "enum": ["success", "failure", "denied", "timeout"]
    },
    "trace_id": {
      "type": "string",
      "minLength": 1
    },
    "autonomy_tier": {
      "type": "string",
      "enum": ["shadow", "suggest", "act_with_approval", "act_auto"]
    },
    "timestamp": {
      "type": "string",
      "format": "date-time"
    },
    "cost_usd": {
      "type": ["number", "null"]
    },
    "chain_index": {
      "type": "integer",
      "minimum": 1
    },
    "previous_hash": {
      "type": ["string", "null"],
      "pattern": "^(sha256:[0-9a-f]{64})$"
    },
    "entry_hash": {
      "type": "string",
      "pattern": "^sha256:[0-9a-f]{64}$"
    },
    "metadata": {
      "type": "object"
    }
  }
}
```

## Sample stream

```json
[
  {
    "schema": "opentrustgraph/v0",
    "record_id": "01966f4c-0f31-7b5d-b44b-f7f8e7e1d384",
    "agent": "github-triage-bot",
    "action": "github.issue.opened",
    "approver": null,
    "outcome": "denied",
    "trace_id": "trace_shadow_01",
    "autonomy_tier": "shadow",
    "timestamp": "2026-04-19T18:42:11Z",
    "cost_usd": null,
    "chain_index": 1,
    "previous_hash": null,
    "entry_hash": "sha256:bd9f5d07cd3185d88cc15b255a491e09b46b7bbdd095b795f45a709e4bb74f8f",
    "metadata": {
      "terminal_status": "failed",
      "reason": "shadow tier blocks direct mutation"
    }
  },
  {
    "schema": "opentrustgraph/v0",
    "record_id": "01966f4c-0f32-7d37-b443-d72dd96f0f4f",
    "agent": "github-triage-bot",
    "action": "trust.promote",
    "approver": "maintainer-1",
    "outcome": "success",
    "trace_id": "trustctl-01966f4c-0f32-7d37-b443-d72dd96f0f4f",
    "autonomy_tier": "act_auto",
    "timestamp": "2026-04-19T18:43:02Z",
    "cost_usd": null,
    "chain_index": 2,
    "previous_hash": "sha256:bd9f5d07cd3185d88cc15b255a491e09b46b7bbdd095b795f45a709e4bb74f8f",
    "entry_hash": "sha256:75955793c1806c9e56248b5b756f5d909ed4f1680c780f83075738c7552b93af",
    "metadata": {
      "control": true
    }
  },
  {
    "schema": "opentrustgraph/v0",
    "record_id": "01966f4c-0f33-79f7-a4a8-82c6900e31f8",
    "agent": "github-triage-bot",
    "action": "github.issue.opened",
    "approver": null,
    "outcome": "success",
    "trace_id": "trace_live_01",
    "autonomy_tier": "act_auto",
    "timestamp": "2026-04-19T18:43:10Z",
    "cost_usd": 0.0041,
    "chain_index": 3,
    "previous_hash": "sha256:75955793c1806c9e56248b5b756f5d909ed4f1680c780f83075738c7552b93af",
    "entry_hash": "sha256:bea6b8da017bc3639ff8c1e8cf704fbbb57a2a662a45639d6fed4b30a538ec41",
    "metadata": {
      "provider": "github",
      "binding_version": 4,
      "terminal_status": "succeeded"
    }
  }
]
```

## Portability notes

- The schema is append-only friendly. New consumers should ignore unknown
  fields inside `metadata`.
- Chain verification is local and deterministic: recompute each `entry_hash`
  with `entry_hash` removed and compare `previous_hash` with the prior record.
- A runtime can project the same record into Kafka, Temporal histories,
  Inngest events, or an internal append-only audit log without changing the
  core contract.
- Promotion and demotion events are ordinary trust records with
  `action = "trust.promote"` or `action = "trust.demote"`, which keeps control
  changes inside the same audit substrate as execution records.
