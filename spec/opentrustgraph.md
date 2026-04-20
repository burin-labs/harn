# OpenTrustGraph v0

`OpenTrustGraph` is a portable event schema for recording autonomy and approval
decisions around agent dispatch. Harn emits these records onto `trust.graph`
plus the per-agent topic `trust.graph.<agent_id>`, but the format is designed to
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
- A runtime can project the same record into Kafka, Temporal histories,
  Inngest events, or an internal append-only audit log without changing the
  core contract.
- Promotion and demotion events are ordinary trust records with
  `action = "trust.promote"` or `action = "trust.demote"`, which keeps control
  changes inside the same audit substrate as execution records.
