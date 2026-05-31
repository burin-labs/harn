# Pool stdlib

`std/lifecycle/pool` provides named, concurrency-bounded **agent worker pools**.
Use a pool when work needs to share a single concurrency budget across an
entire pipeline, session, or tenant — for example, capping how many PR-review
agents run at once, fairly draining a per-customer queue, or throttling a
provider's API tier.

Pool workers run as child-VM isolates. On a multi-thread Tokio runtime they are
scheduled with `tokio::spawn`, while the pool registry is scoped through the
VM so nested pool operations see the same named budgets.

```harn,ignore
import { fair_round_robin, pool_create, pool_wait } from "std/lifecycle/pool"

let pool = pool_create({
  name: "pr-review",
  max_concurrent: 5,
  queue: fair_round_robin("tenant_id"),
})

let handle = pool.submit({ ->
  return agent_loop("review this PR", system_prompt: "...")
}, {tenant_id: "tenant-acme", priority: 10})

let result = pool_wait(handle)
```

## Creating a pool

`pool_create(options?)` allocates a new pool and registers it under
`options.name`. Names must be unique within the live VM registry; use
`pool_get(name)` to reuse an existing one. Pipeline-scope pools use a
deterministic id so creating the same `(scope, scope_id, name)` after process
restart binds to the persisted state.

| Option           | Type   | Default          | Notes                                          |
|------------------|--------|------------------|------------------------------------------------|
| `name`           | string | auto-generated   | Visible in `pool_list()` and snapshots.        |
| `max_concurrent` | int    | `1`              | Hard cap on simultaneously running tasks.      |
| `queue`          | dict/string | `priority()` | Queue strategy descriptor. |
| `backpressure`   | dict/string | `nil`        | Backpressure descriptor. `nil` keeps the queue unbounded. |
| `scope`          | string | `"session"`      | `"session"`, `"pipeline"`, `"tenant"`, or `"org"`. |
| `pipeline_id`    | string | active pipeline id | Required for `scope: "pipeline"` outside a pipeline. |
| `stale_after_ms` | int/duration | `30000` | Pipeline-scope freshness knob for persisted running-task markers on reload. |

The returned handle is a dict with `_type: "pool"`, plus `submit`, `size`, and
`snapshot` callable fields that close over the pool's id.

## Submitting work

`pool.submit(closure, options?)` enqueues a zero-arg closure. The pool
spawns a worker the moment a slot is free; everything else queues. Submit-
time options:

| Option     | Type   | Default | Notes                                                            |
|------------|--------|---------|------------------------------------------------------------------|
| `priority` | int    | `0`     | Higher dequeues sooner; ties resolve by submission order (FIFO). |
| `key`      | string | nil     | Fairness key for `fair_round_robin("key")` and task observability. |
| custom key | string | nil     | When using `fair_round_robin("tenant_id")`, pass `tenant_id` here. |
| `idempotency_key` | string | nil | Duplicate submits with the same key return the same task handle. |

Each call returns a task handle (`_type: "pool_task"`) with `id`, `pool`,
`pool_id`, `submitted_at`, `status`, and the optional `key`. Drop policies
return handles whose terminal snapshot has `status: "rejected"`,
`rejection_reason`, and `rejection_policy`.

## Queue Strategies

`std/lifecycle/pool` exports four strategy factories:

| Function | Behavior |
|---|---|
| `fifo()` | Dequeue oldest queued task first, ignoring submit priority. |
| `priority()` | Dequeue highest submit-time `priority` first; equal priorities stay FIFO. This is the default. |
| `lifo()` | Dequeue newest queued task first. |
| `fair_round_robin(key = "key")` | Partition queued tasks by the submit option named by `key`, then rotate across distinct values while preserving FIFO inside each partition. |

```harn,ignore
import { QueueStrategy, pool_create } from "std/lifecycle/pool"

let queue = QueueStrategy()
let pool = pool_create({
  name: "tenant-work",
  max_concurrent: 2,
  queue: queue.fair_round_robin("tenant_id"),
})
```

## Backpressure

`std/lifecycle/pool` exports three backpressure factories and the
`Backpressure()` namespace helper:

| Function | Behavior |
|---|---|
| `backpressure_queue(max_depth, on_full = "block_submitter")` | Bounds queued tasks. `on_full` is `block_submitter`, `drop_oldest`, `drop_newest`, or `fail_submitter`. |
| `fail_fast()` | Rejects any submit that cannot start immediately; no queue is retained. |
| `ring_buffer(capacity)` | Retains the newest queued tasks by dropping the oldest queued task on overflow. |

```harn,ignore
import { Backpressure, pool_create } from "std/lifecycle/pool"

let backpressure = Backpressure()
let pool = pool_create({
  name: "review",
  max_concurrent: 2,
  backpressure: backpressure.queue(100, "fail_submitter"),
})
```

`fail_submitter` raises `HARN-POL-001`; `fail_fast` raises
`HARN-POL-002`. `drop_oldest`, `drop_newest`, and `ring_buffer` emit a
`pool_drop` audit event on the `lifecycle.pool.audit` EventLog topic with
the pool name, task ids, policy, queue depth, and max depth.

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
  `completed`, `failed`, `rejected`, `blocked_submitters`, `total`,
  the selected `backpressure`, the per-task list, and the original
  `config` so observability stacks can show "what was configured".
- `pool_get(name_or_id)` — lookup by name; returns `nil` when missing.
- `pool_list()` — every pool registered on the current VM runtime.

## Composability

- **With `wait_agent`** — pool task handles route through the same agent
  waiter, so user code does not need to learn a second waiter API.
- **With `parallel each`** — the pool's `max_concurrent` is a VM-scoped
  cap; `parallel each ... with { max_concurrent: N }` remains the right tool
  for a per-call-site bound.
- **With trigger routing** — `SpawnToPool` trigger handlers submit matched
  events into a named pool without bypassing queue strategy or backpressure.
- **With pipeline scope** — the JSONL store keeps terminal and stale task
  records inspectable after restart; closure bodies are not journalled, so
  callers submit fresh work when they want a retry attempt.
