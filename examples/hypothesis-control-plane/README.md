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
