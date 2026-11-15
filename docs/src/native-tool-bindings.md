# Native tool bindings reference

A native tool binding attaches an independently compiled Harn contract to an
ordinary MCP stdio worker. Its executable handlers enter the same tool registry
used by Harn's CLI, MCP server, and agents.

## Registry construction

`std/tools.native_tool_registry(tools, spec, ir, max_concurrency)` returns a
`ToolRegistry`. `tools` is `harness.tools`. `ir` is a validated
[code-generation IR](./code-generation.md) produced independently of the worker.
`max_concurrency` is a positive integer that must exactly match the worker profile.

`NativeMcpProcessSpec` contains:

| Field | Contract |
| --- | --- |
| `program` | Host-selected executable. |
| `args` | Optional literal string argument list. No shell expansion. |
| `cwd` | Explicit working directory admitted by the execution policy. |
| `environment` | Optional complete string environment; defaults to empty. |

The host selects process authority. Catalog contents and worker responses can't
choose an executable, environment, or filesystem roots. The process uses Harn's
existing sandbox and process containment. Closing its connection retires and
reaps the owned process through the same lifetime owner.

The registry retains the public catalog's schemas, governance, CLI metadata,
annotations, application errors, and task support. Its closures retain the
verified connection. `tool_registry_bind(catalog, handlers)` is the pure inverse
projection: it requires exactly one closure per catalog operation and refuses
metadata that can't round-trip through the VM exactly.

## Verification and invocation

Before returning a native connection, Harn validates the IR, reads ordinary MCP
discovery and tool listing, and verifies all three contract digests. The worker
publishes its closed `harn-tools-mcp/1.0` profile in the `server/discover` result's
`_meta["com.harnlang/nativeBinding"]`, beside standard server information.

The worker tool list must exactly equal Harn's prepared unary MCP projection.
Harn requires protocol `2026-07-28` and the exact operation and concurrency census.
A missing profile, mismatched digest, unexpected tool, or paginated list refuses
the binding before any tool handler receives a call.

Each invocation validates and decodes input before dispatch, then validates and
decodes its result. The IR explicitly records whether MCP `structuredContent` is
bare or boxed under `result`. A record field named `result` doesn't select an
envelope. JSON-RPC faults, malformed replies, missing structured output, and
invalid output remain failures. Declared application data remains distinct from
those protocol and runtime failures.

Concurrent calls share one MCP peer and retain request-specific response and
cancellation accounting. Dropping one pending call sends ordinary stdio
`notifications/cancelled` for that request. It doesn't cancel an unrelated call.
Raw compatibility requests can't be mixed into an activated native connection.

Each stdio message is limited to 16 MiB of UTF-8 bytes, including its LF
terminator and any preceding CR. The generated protocol facts export both the
limit and that terminator rule. Worker stderr is drained without retaining or
publishing its contents; ordinary diagnostics don't impose a cumulative worker
lifetime quota.

Native adapter failures prevent automatic replay of the operation, including
protocol faults and rejected output after a handler may have mutated state.
The original failure category and caught value remain available. A caller can
make a deliberate new call after investigating the outcome.

The worker transport is unary. It can't return `task` or `input_required` in
place of a typed result. The public Harn adapter owns
[long-running tool tasks](./long-running-tools.md), invocation preparation, and
governance. SDK workers don't implement another Harn task store.

## HTTP cancellation

For protocol `2026-07-28`, closing an active HTTP response cancels that direct
`tools/call`, for JSON responses and SSE streams. Harn suppresses later progress
and terminal responses for that request. Cancellable blocking operations observe
the matching call's interruption.

A task returned by `tools/call` has a separate lifetime. Closing that response
doesn't cancel the task. Send `tasks/cancel` with its `taskId` to request
cancellation. Read `tasks/get` to report its terminal state. Neither the
acknowledgement nor the terminal state proves handler or process cleanup has
finished.

These rules follow the MCP [Streamable HTTP transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
and [task cancellation](https://tasks.extensions.modelcontextprotocol.io/specification/2026-07-28/tasks#task-cancellation)
contracts. Stdio uses request-specific cancellation notifications.
