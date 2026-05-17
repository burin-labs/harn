# Audit receipts

Harn owns one canonical audit receipt envelope for run, persona, tool, model,
approval, handoff, and side-effect summaries:

- Rust type: `harn_vm::receipts::Receipt`
- Rust sink trait: `harn_vm::receipts::ReceiptSink`
- JSON Schema: `docs/schemas/receipt.v1.json`
- Schema discriminator: `harn.receipt.v1`

Any new receipt-emitting code MUST serialize this envelope directly or derive
from it. Downstream crates and applications MUST NOT define parallel receipt
structs with their own `parent_run_id`, `trace_id`, and `cost_usd` field set.
If a surface needs a smaller public view, build a projection from
`Receipt` instead of inventing another source shape.

## Envelope

The v1 envelope includes:

- `id`, `parent_run_id`, `persona`, optional `step`, and `trace_id`
- `started_at`, optional `completed_at`, and `status`
- optional `inputs_digest` and `outputs_digest`
- `model_calls[]`, `tool_calls[]`, `approvals[]`, `handoffs[]`, and
  `side_effects[]`
- `cost_usd`, optional `error`, `redaction_class`, and extensible `metadata`

Digest fields are content-addressed references, not raw payload storage. Keep
secret-bearing inputs, outputs, and tool arguments out of the receipt body; put
their hash in the digest field and store restricted material behind the
appropriate host-controlled artifact path. Hosts that opt into the unified
redaction policy can wrap their existing `ReceiptSink` with
`RedactingReceiptSink` to scrub auth headers, URLs with credentials, and
known secret patterns from `model_calls[]`, `tool_calls[]`, and `metadata`
before persistence — see [Redaction policy](./redaction.md).

## Rust usage

```rust
use harn_vm::receipts::{Receipt, ReceiptSink, ReceiptStatus};

let receipt = Receipt::new("receipt_01", "merge_captain", "trace_01", started_at)
    .completed(completed_at, ReceiptStatus::Success);

receipt.validate_required_shape()?;
sink.persist_receipt(&receipt).await?;
```

Implement `ReceiptSink` at persistence boundaries. The trait keeps storage
backends such as harn-cloud-store, local run-record writers, and future host
surfaces accepting the same envelope even when their physical storage differs.

## harn-cloud migration

`run_receipts.payload` remains JSONB. The migration should enforce the canonical
envelope at the JSON boundary:

```sql
ALTER TABLE run_receipts
  ADD COLUMN payload JSONB;

ALTER TABLE run_receipts
  ADD CONSTRAINT run_receipts_payload_receipt_v1
  CHECK (
    payload IS NULL OR (
      jsonb_typeof(payload) = 'object'
      AND payload->>'schema' = 'harn.receipt.v1'
      AND payload ? 'parent_run_id'
      AND payload ? 'trace_id'
      AND payload ? 'cost_usd'
      AND jsonb_typeof(payload->'model_calls') = 'array'
      AND jsonb_typeof(payload->'tool_calls') = 'array'
      AND jsonb_typeof(payload->'approvals') = 'array'
      AND jsonb_typeof(payload->'handoffs') = 'array'
      AND jsonb_typeof(payload->'side_effects') = 'array'
    )
  );
```

The existing typed harn-cloud receipt views can stay as projections for public
or operator display, but writes to `payload` should come from
`harn_vm::receipts::Receipt` and the schema in `docs/schemas/receipt.v1.json`.

## burin-code migration

`RunRecord` readers in burin-code should be generated from
`docs/schemas/receipt.v1.json` or decode the canonical envelope directly. Swift
view models may keep local projection types, but they should map from the
schema-generated receipt type and not carry independent field definitions for
`parent_run_id`, `trace_id`, and `cost_usd`.

## Duplication guard

`make check-receipt-structs` fails if a Rust `pub struct` outside
`crates/harn-vm/src/receipts/` contains the duplicate receipt field trio
`parent_run_id`, `trace_id`, and `cost_usd`. `make check` runs the guard through
the `all` target.
