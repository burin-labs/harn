# `harness.agent.current_id()`

Return the innermost active agent session id for the currently executing
VM thread.

## Signature

```harn,ignore
harness.agent.current_id() -> string | nil
```

The builtin returns:

- the active `session_id` while code is running inside an `agent_loop(harness, ...)`
  turn, subscriber callback, or other session-scoped callback
- `nil` when no agent session is active

## Why it exists

Session management builtins like `harness.agent.snapshot(id)`,
`harness.agent.fork(id, dst?)`, and `harness.agent.trim(id, keep_last)`
operate on explicit ids. `harness.agent.current_id()` lets nested handlers
discover "the session I am currently executing under" without threading that
id through every layer manually.

## Example

```harn
const session = "support-thread"

agent_subscribe(
  session,
  { ev ->
  if ev?.type == "iteration_end" {
    const current = harness.agent.current_id()
    if current != nil {
      agent_inject_feedback(current, "iteration_marker", "just finished an iteration")
    }
  }
},
)
```

Use [Sessions](../sessions.md) for the broader storage and lifecycle model.
