# OpenTrustGraph Spec

This directory is the repo-ready seed for the standalone `opentrustgraph-spec`
repository. It contains the normative v0 JSON Schema and conformance fixtures
used by Harn's trust-graph runtime.

OpenTrustGraph records autonomy decisions as append-only, hash-chained events.
Each record captures the agent, action, optional approver, outcome, trace id,
effective autonomy tier, and runtime metadata.

## Contents

- `schemas/trust-record.v0.schema.json`: JSON Schema for one v0 trust record.
- `fixtures/valid/decision-chain.json`: a valid two-entry hash chain.
- `fixtures/invalid/tampered-chain.json`: a chain with an invalid previous hash.

Consumers should validate each record against the schema and then verify the
chain by recomputing `entry_hash` over the canonical record with `entry_hash`
removed and comparing each `previous_hash` to the prior entry.
