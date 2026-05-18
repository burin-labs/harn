# Pool stdlib

`std/lifecycle/pool` provides named, concurrency-bounded **agent thread pools**.
Use a pool when work needs to share a single concurrency budget across an
entire pipeline, session, or tenant — for example, capping how many PR-review
agents run at once, fairly draining a per-customer queue, or throttling a
provider's API tier.

This is PL-01, the foundation for the agent-pool epic
([#1883](https://github.com/burin-labs/harn/issues/1883)). Queue strategies,
backpressure policies, channel composition, durable state, and OTel spans land
in [#1887..#1893](https://github.com/burin-labs/harn/issues?q=is%3Aissue+label%3Aarea%2Fflow-control).

```harn,ignore
import { pool_create, pool_wait } from "std/lifecycle/pool"

let pool = pool_create({name: "pr-review", max_concurrent: 5})

let handle = pool.submit({ ->
  return agent_loop("review this PR", system_prompt: "...")
}, {key: "tenant-acme", priority: 10})

let result = pool_wait(handle)
```

## Creating a pool

`pool_create(options?)` allocates a new pool and registers it under
`options.name`. Names must be unique within the runtime — re-creating a
pool errors; use `pool_get(name)` to reuse an existing one.

| Option           | Type   | Default          | Notes                                          |
|------------------|--------|------------------|------------------------------------------------|
| `name`           | string | auto-generated   | Visible in `pool_list()` and snapshots.        |
| `max_concurrent` | int    | `1`              | Hard cap on simultaneously running tasks.      |
| `queue`          | any    | `nil` (FIFO)     | Reserved for [#1887](https://github.com/burin-labs/harn/issues/1887). |
| `backpressure`   | any    | `nil`            | Reserved for [#1888](https://github.com/burin-labs/harn/issues/1888). |
| `priority`       | any    | `nil`            | Reserved for [#1887](https://github.com/burin-labs/harn/issues/1887). |

The returned handle is a dict with `_type: "pool"`, plus `submit`, `size`, and
`snapshot` callable fields that close over the pool's id.

## Submitting work

`pool.submit(closure, options?)` enqueues a zero-arg closure. The pool
spawns a worker the moment a slot is free; everything else queues. Submit-
time options:

| Option     | Type   | Default | Notes                                                            |
|------------|--------|---------|------------------------------------------------------------------|
| `priority` | int    | `0`     | Higher dequeues sooner; ties resolve by submission order (FIFO). |
| `key`      | string | nil     | Stamped on the task for observability and future fair-key queues. |

Each call returns a task handle (`_type: "pool_task"`) with `id`, `pool`,
`pool_id`, `submitted_at`, and the optional `key`.

## Waiting

`pool_wait(handle)` blocks until the task reaches a terminal state and
returns the final task snapshot (`status`, `result` or `error`, timestamps).
Passing a list of handles waits for all of them. The same dispatch also
works through `wait_agent(handle)` from `std/agent/workers` — pool task
handles are recognised transparently so `wait_agent` is the one place
callers need to learn.

```harn,ignore
let handles = [pool.submit(work_a), pool.submit(work_b), pool.submit(work_c)]
let outcomes = pool_wait(handles)  // or: wait_agent(handles)
```

## Inspection

- `pool.size()` — count of active + queued tasks (does not include
  terminal-state tasks).
- `pool.snapshot()` — full dict including `active`, `queued`,
  `completed`, `failed`, `total`, the per-task list, and the original
  `config` so observability stacks can show "what was configured".
- `pool_get(name_or_id)` — lookup by name; returns `nil` when missing.
- `pool_list()` — every pool registered on the current runtime.

## Composability

- **With `wait_agent`** — pool task handles route through the same agent
  waiter, so user code does not need to learn a second waiter API.
- **With `parallel each`** — the pool's `max_concurrent` is a process-wide
  cap; `parallel each ... with { max_concurrent: N }` remains the right tool
  for a per-call-site bound.
- **With future siblings** — channel handlers
  ([#1889](https://github.com/burin-labs/harn/issues/1889)) will route trigger
  events through a named pool, and durable state
  ([#1890](https://github.com/burin-labs/harn/issues/1890)) will let
  pipeline-scoped pools survive process restarts. Both build on the registry
  shipped here without changing the user-facing API.
