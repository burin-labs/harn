# OpenTrustGraph v0 conformance

This document defines what a runtime, consumer, or verifier must do to be
"OpenTrustGraph v0 conformant". The wording follows RFC 2119: MUST, MUST
NOT, SHOULD, MAY.

The normative artifacts are:

- `schemas/trust-record.v0.schema.json` — JSON Schema for a single record.
- `schemas/trust-chain.v0.schema.json` — JSON Schema for a chain export.
- `schemas/trust-record.v0.proto` — Protocol Buffers wire format mirror.
- `fixtures/valid/*.json` — chains that MUST validate.
- `fixtures/invalid/*.json` — chains that MUST be rejected.

JSON is the canonical interchange and the basis for hash computation.
Protobuf is provided for streaming runtimes; implementations MUST
round-trip a record through canonical JSON before computing or comparing
`entry_hash` values.

## 1. Producer requirements

A producer is any system that appends records to an OpenTrustGraph stream.

1. Each appended event MUST validate against
   `schemas/trust-record.v0.schema.json`.
2. `record_id` MUST be globally unique within the stream. UUIDv7 is
   RECOMMENDED so identifiers sort by creation time.
3. `chain_index` MUST be 1-based and strictly increasing by 1 between
   adjacent records on the same chain topic.
4. `previous_hash` MUST equal the prior record's `entry_hash`. The first
   record on a chain MUST have `previous_hash = null`.
5. `entry_hash` MUST be computed by:
   1. Building a canonical JSON form of the record with `entry_hash`
      removed.
   2. Sorting object keys lexicographically (UTF-8 byte order) at every
      nesting level — top-level record, every nested object inside
      `metadata`, every approval signature receipt, and so on. Array
      element order MUST be preserved as authored.
   3. Emitting the JSON with no insignificant whitespace
      (`separators=(',', ':')` in Python; the default
      `serde_json::to_string` output in Rust). UTF-8 strings MUST NOT be
      escape-encoded as `\uXXXX` when they are valid in JSON; producers
      SHOULD emit them verbatim to match the reference impl.
   4. Hashing the resulting bytes with SHA-256.
   5. Storing the hex digest with a `sha256:` prefix.

   The Harn reference impl achieves the same shape by serializing
   through `serde_json::Value` (which is backed by `BTreeMap`, hence the
   alphabetical key order) and removing the `entry_hash` key before
   hashing. External producers can replicate this exactly with the
   recursion in `examples/python/verify_chain.py`.
6. When `outcome = "success"`, `autonomy_tier = "act_with_approval"`,
   and `metadata.approval.required = true`, the producer MUST populate a
   non-empty `approver` and at least one signature receipt under
   `metadata.approval.signatures` with `reviewer`, `signed_at`, and
   `signature` fields.
7. Producers SHOULD emit autonomy tier changes as ordinary records with
   `action = "trust.promote"` or `action = "trust.demote"` and
   `metadata.control = true`, so control changes share the audit
   substrate with execution records.
8. Producers MUST NOT mutate or delete records once appended. Corrections
   MUST be expressed as new records that reference the corrected
   `record_id` in `metadata`.

## 2. Consumer requirements

A consumer is any system that reads records or chain exports.

1. Consumers MUST treat unknown fields under `metadata.*` as opaque and
   preserve them when re-emitting records.
2. Consumers MUST NOT reject records solely because they contain unknown
   `metadata` keys.
3. When verifying a chain, consumers MUST:
   1. Validate the export against `schemas/trust-chain.v0.schema.json`.
   2. Validate every record against `schemas/trust-record.v0.schema.json`.
   3. Recompute `entry_hash` for every record using the producer rule
      above and compare it to the stored value.
   4. Compare each record's `previous_hash` to the prior record's
      `entry_hash`.
   5. Compare `chain.total` to the record count and `chain.root_hash` to
      the final record's `entry_hash`.
4. Consumers MAY use the `schema` discriminator to dispatch between
   future versions. Encountering an unknown schema version MUST be a
   non-fatal warning; the consumer SHOULD skip the record and continue.

## 3. Chain export envelope

`opentrustgraph-chain/v0` is the portable envelope for receipts,
supervision UIs, and third-party verifiers.

1. The envelope MUST validate against
   `schemas/trust-chain.v0.schema.json`.
2. `chain.topic` MUST identify the source stream (e.g. `trust_graph`).
3. `chain.verified` SHOULD be `true` only if the producer has run the
   full verification routine in §2.3 against the records it is exporting.
4. `chain.producer.name` and `chain.producer.version` MUST identify the
   emitting runtime so verifiers can attribute compatibility issues.
5. Empty exports are valid: `chain.total = 0`, `chain.root_hash = null`,
   `records = []`.

The Harn reference impl emits this envelope via
`harn trust-graph export --output chain.json` (or stdout). External
consumers can also project the portal `GET /api/trust-graph` response
into the envelope shape.

## 4. Hash contract test vectors

The fixtures under `fixtures/valid/` are normative test vectors. A
conformant verifier MUST accept them and a conformant producer MUST be
able to reproduce identical `entry_hash` and `root_hash` values when
appending the same logical records.

The fixtures under `fixtures/invalid/` are negative test vectors. A
conformant verifier MUST reject them and SHOULD report an error that
mentions either `previous_hash mismatch`, `entry_hash mismatch`, or
`approval required` so operators can triage failures quickly.

## 5. Versioning

- The schema version moves with the on-disk shape. Adding a new optional
  property is backwards compatible and remains v0.
- Removing a property, renaming an enum, or changing the hash contract
  is a breaking change and MUST bump to `opentrustgraph/v1` (and
  `opentrustgraph-chain/v1` for the envelope).
- Multiple major versions MAY coexist on the same stream; consumers
  dispatch on the `schema` discriminator.
