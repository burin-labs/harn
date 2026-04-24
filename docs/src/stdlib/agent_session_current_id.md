# `agent_session_current_id()`

Return the innermost active agent session id for the currently executing
VM thread.

## Signature

```harn,ignore
agent_session_current_id() -> string | nil
```

The builtin returns:

- the active `session_id` while code is running inside an `agent_loop(...)`
  turn, subscriber callback, or other session-scoped callback
- `nil` when no agent session is active

## Why it exists

Session management builtins like `agent_session_snapshot(id)`,
`agent_session_fork(id, dst?)`, and `agent_session_trim(id, keep_last)`
operate on explicit ids. `agent_session_current_id()` lets nested handlers
discover "the session I am currently executing under" without threading that
id through every layer manually.

## Example

```harn
let session = "support-thread"

agent_subscribe(
  session,
  { ev ->
  if ev?.type == "turn_end" {
    let current = agent_session_current_id()
    if current != nil {
      agent_inject_feedback(current, "turn_marker", "just finished a turn")
    }
  }
},
)
```

Use [Sessions](../sessions.md) for the broader storage and lifecycle model.
