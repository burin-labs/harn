# Use the hypothesis control plane from CLI or MCP

This example projects `std/eval/hypothesis` without changing its contracts.
The CLI and MCP tools call the same planner, deterministic compiler, workflow,
ledger, and report functions.

## Run one CLI action

Create a JSON file for the action:

- `design`: `request`, `context`, and optional `options`
- `compile`: `intent` and `context`
- `apply`: typed workflow `request`
- `inspect` or `report`: `hypothesis_id`

Then run:

```console
harn run examples/hypothesis-control-plane/main.harn -- \
  --action=compile --input=request.json
```

Keep the input under the project root. For an external file, add its directory
with `harn run --read-only-root <directory>`.

`design` can call a model. `apply` can execute work when the host has registered
the `hypothesis.operation` adapter. The compiler and workflow still enforce the
plan's capabilities, placement, approval, and resource ceilings.

## Serve the same functions over MCP

Start the exported functions as structured MCP tools:

```console
harn serve mcp examples/hypothesis-control-plane/main.harn
```

The server exposes `design`, `compile`, `apply`, `inspect`, and `report`. Harn
derives each input schema from the function signature.
An MCP transport does not add authority: `apply` still fails closed when the
native adapter or its scoped grants are absent.

## Dogfood a natural-language hypothesis

This deterministic path sends a natural-language question through the planner,
compiler, native workflow boundary, SQLite ledger, and report. The LLM fixture
and scenario adapter make no provider or network calls.

```console
export HARN_EVENT_LOG_BACKEND=sqlite
export HARN_EVENT_LOG_SQLITE_PATH=/tmp/harn-hypothesis-aa.sqlite
harn test-bench run examples/hypothesis-control-plane/scenario.harn \
  --llm-fixture examples/hypothesis-control-plane/scenario.llm-mock.jsonl \
  --hypothesis-scenario aa -- --scenario=aa
```

Replace both `aa` values with `known-bad`, `denied`, `budget-exhausted`, or
`missing-telemetry` to exercise the corresponding terminal path. Use a fresh
SQLite path for each independent scenario.

To prove recovery across a process boundary, keep the logical scenario and
SQLite path stable while changing only the native adapter fault:

```console
export HARN_EVENT_LOG_SQLITE_PATH=/tmp/harn-hypothesis-recovery.sqlite
harn test-bench run examples/hypothesis-control-plane/scenario.harn \
  --llm-fixture examples/hypothesis-control-plane/scenario.llm-mock.jsonl \
  --hypothesis-scenario fail-decision-attestation -- --scenario=recovery
# The first command fails after persisting completion. Resume from that ledger:
harn test-bench run examples/hypothesis-control-plane/scenario.harn \
  --llm-fixture examples/hypothesis-control-plane/scenario.llm-mock.jsonl \
  --hypothesis-scenario aa -- --scenario=recovery
```

The resumed process appends the missing canonical decision. It does not rerun
or duplicate completed observations.
