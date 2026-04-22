# Runtime Context

Harn exposes logical runtime identity through `runtime_context()`. This is the
task/thread abstraction Harn code should use for observability and debugging;
raw OS thread IDs are not part of the stable language surface.

`task_current()` is an alias for `runtime_context()`.

```harn
let ctx = runtime_context()
println(ctx.task_id)
println(ctx.parent_task_id)
println(ctx.root_task_id)
```

## Stable Fields

These fields are stable API and always present. Unavailable values are `nil`.

| Field | Description |
|---|---|
| `task_id` | Current logical task id |
| `parent_task_id` | Parent logical task id, or `nil` for the root task |
| `root_task_id` | Root logical task id for this execution tree |
| `task_name` | Runtime label such as `root`, `spawn`, `parallel`, `parallel each`, or `parallel settle` |
| `task_group_id` | Shared id for siblings created by one `parallel` operation |
| `scope_id` | Optional host/runtime scope id |
| `workflow_id`, `run_id`, `stage_id`, `worker_id` | Workflow and delegated-worker identity when available |
| `agent_session_id`, `parent_agent_session_id`, `root_agent_session_id`, `agent_name` | Agent-loop session identity when available |
| `trigger_id`, `trigger_event_id`, `binding_key`, `tenant_id`, `provider` | Trigger-dispatch identity when available |
| `trace_id`, `span_id` | Current tracing identity when available |
| `scheduler_key`, `runner`, `capacity_class` | Scheduler/runtime hints when available |
| `context_values` | Task-local values for the current logical task |
| `cancelled` | Whether the current task has observed cancellation |

## Context Values

Task-local values are stored on the current logical task. Children created by
`spawn`, `parallel`, `parallel each`, and `parallel settle` inherit a snapshot of
the parent values. Later child writes do not mutate the parent, and later parent
writes do not affect already-created children.

```harn
runtime_context_set("tenant", "acme")

let result = parallel each ["a", "b"] { item ->
  runtime_context_get("tenant")
}

assert_eq(result, ["acme", "acme"])
```

| Function | Description |
|---|---|
| `runtime_context_values()` | Return the current task-local values as a dict |
| `runtime_context_get(key, default?)` | Return a task-local value, `default`, or `nil` |
| `runtime_context_set(key, value)` | Set a task-local value and return the previous value or `nil` |
| `runtime_context_clear(key)` | Clear a task-local value and return the previous value or `nil` |

## Debug Fields

`runtime_context().debug` contains best-effort introspection for tools:
`active_task_ids`, `waiting_reason`, `cancelled`, and `held_synchronization`.
These fields are intended for diagnostics and may grow as the scheduler gains
more structured-concurrency and synchronization primitives.
