# Harn Agents Protocol Receipt Format

This directory is the canonical v1 receipt-format artifact for the Harn Agents
Protocol. It formalizes receipts as portable proof summaries, distinct from raw
event traces, so Harnesses can expose audit, replay, cost, approval, and
side-effect evidence without exposing every private trace span by default.

Until a standalone specification site exists, the public URL for this artifact
is:

<https://github.com/burin-labs/harn/tree/main/agents-protocol-receipts>

## Version marker

- Receipt envelope: `receipt-2026-04-25`

## Contents

- `schemas/receipt-2026-04-25.schema.json`: JSON Schema for the receipt
  envelope.
- `fixtures/valid/task-receipt.json`: a complete task receipt with approvals,
  model routing, cost, replay material, side effects, final artifacts, and
  hash-chain metadata.
- `fixtures/invalid/missing-chain-hash.json`: an invalid receipt missing the
  required receipt hash.

## Receipt envelope

A receipt is a JSON object with these top-level fields:

- `schema`: version discriminator. Current value: `receipt-2026-04-25`.
- `receipt_id`: stable receipt id.
- `subject`: resource pointer for the Task, Outcome, Event, Artifact, approval,
  tool use, or replay segment the receipt proves.
- `issuer`: Harness identity that issued the receipt.
- `issued_at`: RFC3339 UTC timestamp.
- `identifiers`: tenant, workspace, session, task, persona, and branch
  identifiers known for the run.
- `lifecycle`: start, completion, and final-state data for the subject.
- `approvals`: approval decisions and quorum proof references.
- `trust`: autonomy tier at start and end.
- `autonomy_budget`: budget consumed by model, tool, time, or money dimensions.
- `replay_input`: deterministic replay material references or embedded replay
  records.
- `model_route`: chosen model, alternatives considered, and route-policy
  rationale.
- `cost`: total cost and per-provider breakdown.
- `side_effects`: file-system writes, network egress, tool calls, and A2A
  handoffs.
- `final_artifacts`: final artifact references with stable hashes when bytes
  are available.
- `chain`: previous receipt hash, current receipt hash, and optional Merkle
  root.
- `redactions`: omitted private material with reason and proof hash when
  available.
- `signatures`: issuer or witness signatures over the receipt hash.
- `metadata`: extensible implementation detail bag.

## Canonical JSON form

The storage and transport form is UTF-8 JSON. Producers MUST canonicalize the
JSON data model with RFC 8785 JSON Canonicalization Scheme before hashing or
signing. The `chain.receipt_hash` value is computed over the canonical receipt
with `chain.receipt_hash` and `signatures` removed. The stored value uses the
`sha256:` prefix.

Consumers MUST preserve unknown fields only inside explicitly extensible
objects such as `metadata`, `quorum_proof`, provider-specific cost metadata,
and side-effect metadata. Unknown top-level fields are invalid for this schema
version.

## Verification contract

Consumers should:

1. Validate the receipt against `receipt-2026-04-25.schema.json`.
2. Remove `chain.receipt_hash` and `signatures` from a copy of the receipt.
3. Canonicalize the remaining JSON with RFC 8785.
4. Compute SHA-256 over the canonical bytes and compare it with
   `chain.receipt_hash`.
5. Compare `chain.previous_receipt_hash` with the prior receipt in the stream
   when one is available.
6. Verify `chain.merkle_root` against the server-advertised receipt batch when
   a batch root is present.
7. Verify every signature over `chain.receipt_hash` with the advertised key id.

Receipts can contain replay material directly, but large or sensitive replay
inputs SHOULD be stored as Artifacts and referenced by URI plus hash. Receipts
MUST redact secrets and hidden chain-of-thought. Redacted material SHOULD still
leave stable hashes or artifact references when that is enough to prove replay
completeness.

## CBOR archive encoding

The optional archive encoding is deterministic CBOR over the same JSON data
model using RFC 8949 preferred serialization. The CBOR media type is:

```text
application/vnd.harn.receipt+cbor; schema=receipt-2026-04-25
```

CBOR archives do not change the hash contract. Producers compute
`chain.receipt_hash` from the canonical JSON form, not from CBOR bytes, so JSON
and CBOR encodings of the same receipt verify identically.

## OpenAPI integration

The Agents Protocol OpenAPI component for `Receipt` references the JSON Schema
artifact directly:

```yaml
components:
  schemas:
    Receipt:
      $ref: ../agents-protocol-receipts/schemas/receipt-2026-04-25.schema.json
```

Task, Outcome, Event, and Artifact resources should continue to carry
`receipt_id` references for lightweight reads. Endpoints that return full
receipt bodies should use the shared `Receipt` schema component.
