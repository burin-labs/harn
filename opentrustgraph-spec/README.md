# OpenTrustGraph spec

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

- Trust record: `opentrustgraph/v0.1` (current). `opentrustgraph/v0`
  records still validate for one patch release window per
  [`CONFORMANCE.md` §5](./CONFORMANCE.md#5-versioning).
- Chain export: `opentrustgraph-chain/v0` (unchanged in v0.1 — the bump
  is additive at the record metadata layer).

`v0.1` reserves four lineage keys under `TrustRecord.metadata`:
`actor_chain` (RFC 8693 subject/actor chain for who caused this record),
`effects_grant` (typed effect list the parent extended to this record),
`effects_used` (typed effect list the action actually exercised), and
`parent_record_id` (pointer to the parent record's `record_id`).
Verifiers MUST check that a child's `effects_used` is a subset of the
parent's `effects_grant`; when `actor_chain` is present with
`parent_record_id`, verifiers MUST also check that the child's nested `act`
chain extends the parent's `actor_chain` by exactly one hop.

## Contents

- `CONFORMANCE.md`: RFC 2119 conformance requirements for producers,
  consumers, and verifiers.
- `schemas/trust-record.v0.1.schema.json`: JSON Schema for the current
  v0.1 trust record. Accepts both `opentrustgraph/v0.1` and
  `opentrustgraph/v0` discriminators.
- `schemas/trust-record.v0.schema.json`: JSON Schema for the legacy
  v0 trust record; retained for the back-compat window so consumers can
  validate older records directly.
- `schemas/trust-chain.v0.schema.json`: JSON Schema for a v0 chain export with
  chain metadata and ordered records.
- `schemas/trust-record.v0.proto`: Protocol Buffers wire-format mirror for
  streaming runtimes (Kafka, gRPC, Temporal). JSON remains canonical and is the
  basis for `entry_hash` computation.
- `fixtures/valid/decision-chain.json`: a valid two-entry decision chain.
- `fixtures/valid/tier-transition.json`: a valid chain showing a tier
  transition and approval-backed action.
- `fixtures/invalid/tampered-chain.json`: a chain with a self-consistent record
  hash but invalid previous-hash linkage.
- `fixtures/valid/effect-inheritance-chain.json`: a v0.1 three-agent chain
  (parent → child → grandchild) that demonstrates the effect-inheritance
  invariant `effects_used ⊆ child.effects_grant ⊆ parent.effects_grant`,
  plus `parent_record_id` and `actor_chain` linkage.
- `fixtures/invalid/actor-chain-parentage.json`: a self-consistent v0.1 chain
  whose `actor_chain` no longer extends the referenced parent record.
- `fixtures/invalid/missing-approval.json`: a record that declares approval was
  required but omits the approver/signature evidence.
- `examples/python/verify_chain.py`: reference, stdlib-only verifier in pure
  Python. Validates every fixture and any chain emitted by
  `harn trust-graph export`.

## Verification contract

Consumers should:

1. Validate the export against `trust-chain.v0.schema.json`.
2. Validate each record against `trust-record.v0.schema.json`.
3. Recompute every `entry_hash` over the canonical record with `entry_hash`
   removed.
4. Compare each record's `previous_hash` to the prior record's `entry_hash`.
5. Compare `chain.total` and `chain.root_hash` to the record list.

Harn computes record hashes by serializing the typed `TrustRecord` with
`entry_hash` removed, sorting object keys lexicographically at every nesting
level, and hashing the resulting JSON bytes with SHA-256. The stored value
uses the `sha256:` prefix. See `CONFORMANCE.md` for the full hash contract.

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
