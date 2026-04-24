# OpenTrustGraph Spec

This directory is the canonical OpenTrustGraph v0 artifact for Harn. It is
kept small enough to vendor into Harn today and direct-publish as a standalone
`burin-labs/opentrustgraph-spec` repository later without changing the format.
Until that repository exists, the public URL for the artifact is:

<https://github.com/burin-labs/harn/tree/main/opentrustgraph-spec>

OpenTrustGraph records autonomy decisions as append-only, hash-chained events.
Each record captures the agent, action, optional approver, outcome, trace id,
effective autonomy tier, runtime metadata, and hash-chain position. Chain export
documents wrap those records with enough metadata for Harn Cloud receipts,
supervision UIs, and third-party verifiers to display the chain root without
inventing another envelope.

## Version markers

- Trust record: `opentrustgraph/v0`
- Chain export: `opentrustgraph-chain/v0`

## Contents

- `schemas/trust-record.v0.schema.json`: JSON Schema for one v0 trust record.
- `schemas/trust-chain.v0.schema.json`: JSON Schema for a v0 chain export with
  chain metadata and ordered records.
- `fixtures/valid/decision-chain.json`: a valid two-entry decision chain.
- `fixtures/valid/tier-transition.json`: a valid chain showing a tier
  transition and approval-backed action.
- `fixtures/invalid/tampered-chain.json`: a chain with a self-consistent record
  hash but invalid previous-hash linkage.
- `fixtures/invalid/missing-approval.json`: a record that declares approval was
  required but omits the approver/signature evidence.

## Verification contract

Consumers should:

1. Validate the export against `trust-chain.v0.schema.json`.
2. Validate each record against `trust-record.v0.schema.json`.
3. Recompute every `entry_hash` over the canonical record with `entry_hash`
   removed.
4. Compare each record's `previous_hash` to the prior record's `entry_hash`.
5. Compare `chain.total` and `chain.root_hash` to the record list.

Harn computes record hashes by serializing the typed `TrustRecord` with
`entry_hash` removed and hashing the resulting JSON bytes with SHA-256. The
stored value uses the `sha256:` prefix.

When `metadata.approval.required` is `true` and a successful record runs at
`act_with_approval`, the record must include a non-empty `approver` and at least
one signature receipt in `metadata.approval.signatures`.

## Harn integration points

- Runtime events are emitted to `trust_graph` plus `trust_graph.<agent_id>`.
- `harn trust-graph verify-chain --json` exposes verification metadata that can
  be projected into the chain export shape.
- The portal `GET /api/trust-graph` endpoint returns records, summaries, and
  verification status for local supervision surfaces.
- Harn Cloud receipts and Burin supervision UI planning should link to this
  directory or the future standalone repository instead of describing the format
  informally.
