# HARN-SUS-005 — agent_await_resumption was invoked outside agent_loop structural handling

The model-facing `agent_await_resumption` tool is structural. `agent_loop`
intercepts that tool call, records audit metadata, and turns it into a
suspend checkpoint. Invoking the tool handler directly bypasses that control
flow, so Harn rejects it.

Call `agent_await_resumption(reason, conditions?)` only to build or normalize
the request shape, or let an `agent_loop` turn handle the lifecycle tool call.
