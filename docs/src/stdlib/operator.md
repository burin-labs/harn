# Operator tools

`std/operator` defines one typed contract for private administrative reads,
focused reveals, and mutations. It builds ordinary `ToolDefinitionSpec` values,
so ToolRegistry remains the owner of CLI, dashboard, private-catalog, MCP, and
agent projections.

An adapter audience controls exposure. It does not authenticate an operator or
authorize an effect. The host that owns the data supplies the authenticated
actor from trusted context, checks its own permission scopes, performs the
effect, writes the domain audit row, and persists a canonical
`harn.receipt.v1` envelope. Caller input never contains the actor.
The constructor rejects an exact `actor` property anywhere in the argument
schema and again in raw nested arguments before validation; trusted host
context is its only owner. It recursively closes record-shaped argument and
result schemas while preserving deliberate map-shaped dict fields.

## Constructor

Use `operator_tool` and pass its result to `tool_registry_from`:

```harn,ignore
import { OperatorResult, operator_tool } from "std/operator"
import { schema_refine, schema_strict_object } from "std/schema"
import { tool_registry_from } from "std/tools"

type StatusArgs = {id: string, include_history: bool}
type StatusData = {id: string, state: "active" | "disabled"}

const tools = tool_registry_from([
  operator_tool<StatusArgs, StatusData>({
    operation: "account.status",
    description: "Read status for one opaque account identifier.",
    audiences: ["cli", "catalog", "dashboard"],
    mode: "read",
    input_classification: "id_only",
    output_classification: "id_only",
    args_schema: schema_refine(
      schema_of(StatusArgs),
      schema_strict_object(schema_of(StatusArgs).properties),
    ),
    data_schema: schema_refine(
      schema_of(StatusData),
      schema_strict_object(schema_of(StatusData).properties),
    ),
    handler: { invocation -> read_status(invocation) },
  }),
])
```

The constructor requires:

- explicit, non-empty audiences;
- `read`, `reveal`, or `mutate` mode;
- input and output classifications;
- closed object schemas for operation-specific `args` and `data`;
- a `target_kind` for reveal and mutate tools;
- one typed handler.

First-wave operator tools cannot use `agent` or `mcp` audiences. The registry
also rejects a manually defined `operator.*` tool that relies on ToolRegistry's
compatibility default. This is a defense against bypassing the constructor, not
a substitute for host authorization.

`personal` input and output are valid only for a focused reveal. A generic
operator argument cannot carry a secret or secret reference. Inject secrets as
host capabilities. A successful `secret_reference` output requires a
`restricted_artifact_ref` and requires the generic `data` field to be absent;
the underlying value never belongs in generic JSON, logs, dashboards, or
receipts. A refused result also carries neither data nor an artifact reference.

## Invocation contract

Every invocation has `schema: "harn.operator-invocation.v1"`, a literal mode,
and strictly validated operation-specific `args`.

A reveal also requires an environment, a target with the constructor's literal
kind, an opaque ID, and a bounded non-whitespace reason.

A mutation additionally requires:

- a stable idempotency key reused by retries;
- expected state with a non-negative version, a SHA-256 digest, or both;
- confirmation whose operation, target ID, and environment exactly match the
  normalized invocation.

Validation completes before the handler runs. The effect owner compares the
expected state and owns idempotent persistence. `std/operator` does not create a
database transaction, distributed lock, or authorization decision.

## Result contract

Every `harn.operator-result.v1` result names the operation, outcome, replay
state, declared data classification, and canonical receipt ID. Reveal and
mutation results repeat the exact target. Mutation outcomes are `applied`,
`no_op`, or `refused`; read and reveal use their corresponding outcomes.

`receipt_id` points to the persisted `harn.receipt.v1` envelope. The operator
result is a transport projection, not another receipt format. Store raw
personal or secret values outside the receipt and retain only identifiers,
digests, redacted side-effect summaries, and restricted artifact references.
