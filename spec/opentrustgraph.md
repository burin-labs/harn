# OpenTrustGraph v0

`OpenTrustGraph` is a portable event schema for recording autonomy and approval
decisions around agent dispatch. Harn emits these records onto `trust_graph`
plus the per-agent topic `trust_graph.<agent_id>` (`trust.graph` is still read
for compatibility), but the format is designed to
be runtime-neutral so other schedulers and workflow engines can adopt the same
stream shape.

Version markers:

```json
{"schema":"opentrustgraph/v0"}
```

```json
{"schema":"opentrustgraph-chain/v0"}
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

Approval evidence:

- When `metadata.approval.required` is `true`, `outcome` is `success`, and
  `autonomy_tier` is `act_with_approval`, the record must include a non-empty
  `approver`.
- The same record must include at least one signature receipt in
  `metadata.approval.signatures`.
- Signature receipt objects are intentionally extensible. Harn uses
  `reviewer`, `signed_at`, and `signature` fields today.

## Chain export

A `TrustChainExport` wraps ordered records with metadata for receipts,
supervision UIs, and third-party verification:

```json
{
  "schema": "opentrustgraph-chain/v0",
  "chain": {
    "topic": "trust_graph",
    "total": 2,
    "root_hash": "sha256:6bb2b155ba07c67443c881f2d9dd954083bb44542df81520db1490fcbfdd5bf9",
    "verified": true,
    "generated_at": "2026-04-19T18:45:00Z",
    "producer": {
      "name": "harn",
      "version": "0.7.x"
    },
  },
  "records": []
}
```

Fields:

- `schema`: chain-export schema/version discriminator. Current value:
  `opentrustgraph-chain/v0`.
- `chain.topic`: canonical event-log topic or exported stream name.
- `chain.total`: number of records in the ordered export.
- `chain.root_hash`: final record's `entry_hash`, or `null` for an empty
  export.
- `chain.verified`: whether the producer verified record hashes, previous-hash
  linkage, and required approval evidence before export.
- `chain.generated_at`: RFC3339 UTC timestamp for the export.
- `chain.producer`: producer name and version.
- `records`: ordered `TrustRecord` list.

## JSON Schema

The normative JSON Schema files live in the public artifact directory:

- [`opentrustgraph-spec/schemas/trust-record.v0.schema.json`](../opentrustgraph-spec/schemas/trust-record.v0.schema.json)
- [`opentrustgraph-spec/schemas/trust-chain.v0.schema.json`](../opentrustgraph-spec/schemas/trust-chain.v0.schema.json)

Harn tests parse those schema files and all fixtures directly, so the spec
artifact and runtime hash contract stay in sync.

## Sample export

```json
{
  "schema": "opentrustgraph-chain/v0",
  "chain": {
    "topic": "trust_graph",
    "total": 2,
    "root_hash": "sha256:6bb2b155ba07c67443c881f2d9dd954083bb44542df81520db1490fcbfdd5bf9",
    "verified": true,
    "generated_at": "2026-04-19T18:45:00Z",
    "producer": {
      "name": "harn",
      "version": "0.7.x"
    }
  },
  "records": [
    {
      "schema": "opentrustgraph/v0",
      "record_id": "01966f4c-0f31-7b5d-b44b-f7f8e7e1d384",
      "agent": "github-triage-bot",
      "action": "github.issue.opened",
      "approver": null,
      "outcome": "success",
      "trace_id": "trace_valid_01",
      "autonomy_tier": "suggest",
      "timestamp": "2026-04-19T18:42:11Z",
      "cost_usd": null,
      "chain_index": 1,
      "previous_hash": null,
      "entry_hash": "sha256:84facae7d56fd304e040ea18d80bd019e274ad86ddd5a4d732f3ac3d984c48ec",
      "metadata": {
        "provider": "github"
      }
    },
    {
      "schema": "opentrustgraph/v0",
      "record_id": "01966f4c-0f32-7d37-b443-d72dd96f0f4f",
      "agent": "github-triage-bot",
      "action": "trust.promote",
      "approver": "maintainer-1",
      "outcome": "success",
      "trace_id": "trace_valid_02",
      "autonomy_tier": "act_auto",
      "timestamp": "2026-04-19T18:43:02Z",
      "cost_usd": null,
      "chain_index": 2,
      "previous_hash": "sha256:84facae7d56fd304e040ea18d80bd019e274ad86ddd5a4d732f3ac3d984c48ec",
      "entry_hash": "sha256:6bb2b155ba07c67443c881f2d9dd954083bb44542df81520db1490fcbfdd5bf9",
      "metadata": {
        "control": true,
        "from_tier": "suggest",
        "to_tier": "act_auto"
      }
    }
  ]
}
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
