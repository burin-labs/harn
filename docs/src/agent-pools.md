# Agent pools

Agent pools are named, concurrency-bounded worker pools for agent work.
Use a pool when many independently-submitted tasks need to share **one**
concurrency budget — capping how many PR-review agents run at once,
fairly draining a per-customer queue, throttling a provider's API tier,
or routing trigger events through a shared dispatcher.

This page covers the pool surface end to end. For a one-screen LLM
quickref see the "Agent pools" section in
`docs/llm/harn-quickref.md`. For runnable patterns see
[Pool cookbook](./cookbooks/pools.md). The stdlib API reference (every
exported builtin, option dict, and return shape) lives at
[Pool stdlib](./stdlib/lifecycle-pool.md).

## When to use a pool

Pools are one of several ways to bound concurrency in Harn. Pick the
narrowest one that fits.

| Goal | Use | Why |
|---|---|---|
| Bound concurrency at one call site | `parallel each ... with { max_concurrent: N }` | Local, no registry, lives and dies with the parallel block. |
| Bound concurrency across many call sites in one VM session | **Pool**, scope `"session"` | One named budget shared by every `pool.submit` using the VM's registry. No durability. |
| Bound concurrency across pipeline runs that survive restart | **Pool**, scope `"pipeline"` | JSONL state in `.harn/pools/` survives crashes; stale tasks remain inspectable on next start. |
| Bound concurrency across tenants/orgs | **Pool**, scope `"tenant"` / `"org"` | Host-managed. Accepted at API level; the in-process runtime rejects it unless an embedding host provides tenant/org routing. |
| Route trigger events through a shared concurrency budget | **Pool** + `SpawnToPool` handler | One trigger per provider, one pool draining them at a fixed rate. |
| Block on a cap inside one task | A semaphore (`std/sync`) | Pools are *task-shaped*; semaphores are *thread-shaped*. |

Three anti-patterns worth calling out:

- **Pools are not message queues.** They submit *closures*, not data.
  If you need a typed mailbox between tasks, use the in-process
  `channel(...)` primitive (see `docs/src/concurrency.md`).
- **Pools are not durable workflows.** Pipeline-scope pools persist
  *queued* and *in-flight* state so the pool survives restart, but the
  task closures themselves are not journalled. A task killed mid-run
  reloads as a failed stale marker; callers that want a retry submit
  fresh work explicitly. Idempotent task bodies are your responsibility.
- **Pools are not fan-out triggers.** One submit runs one closure.
  Use a `channel.emit` trigger when many subscribers should react to
  one event.

## Create a pool

`pool_create(options?)` allocates a new pool, registers it under
`options.name`, and returns a handle:

```harn,ignore
import { pool_create } from "std/lifecycle/pool"

let pool = pool_create({
  name: "pr-review",
  max_concurrent: 5,
})
```

Names must be unique within the live VM registry. Use `pool_get(name)`
to reuse an existing pool in the same process. Pipeline-scope pools use
a deterministic `(scope, scope_id, name)` id, so after a process restart
creating the same pool binds to the persisted state.

The returned handle is a dict with:

```text
{
  _type: "pool",
  id: string,                 // deterministic per (scope, scope_id, name)
  name: string,
  max_concurrent: int,
  scope: "session" | "pipeline" | "tenant" | "org",
  created_at: string,
  // callable methods (closures over the pool id)
  submit: (closure, options?) -> task_handle,
  size: () -> int,
  snapshot: () -> dict,
}
```

### Options

| Option | Type | Default | Notes |
|---|---|---|---|
| `name` | string | auto-generated | Visible in `pool_list()` and snapshots. Used for `pool_get(name)`. |
| `max_concurrent` | int | `1` | Hard cap on simultaneously running tasks. Must be `>= 1`. |
| `queue` | dict / string | `priority()` | Queue strategy descriptor (see [Queue strategies](#queue-strategies)). |
| `backpressure` | dict / string | unbounded | Backpressure descriptor (see [Backpressure](#backpressure)). `nil` keeps the queue unbounded. |
| `scope` | string | `"session"` | Durability scope: `"session"`, `"pipeline"`, `"tenant"`, or `"org"`. |
| `pipeline_id` | string | active pipeline id | Required for `scope: "pipeline"` when not running inside a pipeline. |
| `stale_after_ms` | int / duration | `30000` | Pipeline scope: freshness knob for persisted running-task markers on reload. |

`pool_get(name_or_id)` looks up an existing pool and returns `nil` when
not found. `pool_list()` returns every pool registered in the current
runtime.

## Submit work

`pool.submit(closure, options?)` enqueues a zero-arg closure. The pool
spawns a worker the moment a slot is free; everything else queues
according to the pool's queue strategy and backpressure policy.
On a multi-thread Tokio runtime, those workers run as `tokio::spawn`ed
child-VM isolates with their own execution stacks and the same VM-scoped
pool registry handle.

```harn,ignore
let handle = pool.submit({ ->
  return agent_loop("review this PR", "You are a careful reviewer.")
}, {
  priority: 10,
  tenant_id: "acme",
  idempotency_key: "review-pr-1984",
})
```

### Submit options

| Option | Type | Notes |
|---|---|---|
| `priority` | int | Higher dequeues sooner under `priority()`. Defaults to `0`. Ignored by other strategies (still recorded). |
| `key` | string | Generic fairness key under `fair_round_robin("key")`. Also stamped on the task snapshot for observability. |
| custom key (e.g. `tenant_id`) | string | When using `fair_round_robin("tenant_id")`, pass the partition value under that name. |
| `idempotency_key` | string | Two submits with the same key return the *same* task handle. The second call short-circuits to the first task's terminal snapshot. |

### Task handle

Each `submit` returns a task handle (`_type: "pool_task"`):

```text
{
  _type: "pool_task",
  id: string,
  pool: string,
  pool_id: string,
  submitted_at: string,
  status: "queued" | "running" | "completed" | "failed" | "rejected",
  key: string | nil,
  priority: int,
  // populated when terminal:
  result: any,
  error: string | nil,
  rejection_reason: string | nil,   // when status == "rejected"
  rejection_policy: string | nil,   // "fail_fast" | "fail_submitter" | "drop_oldest" | "drop_newest"
}
```

Rejected handles never run. The terminal snapshot carries
`rejection_reason` and `rejection_policy` so callers can branch without
catching an error.

## Wait on a task

`pool_wait(handle)` blocks until the task reaches a terminal state and
returns the final task snapshot. Passing a list of handles waits for
all of them:

```harn,ignore
import { pool_wait } from "std/lifecycle/pool"

let handles = [pool.submit(work_a), pool.submit(work_b), pool.submit(work_c)]
let outcomes = pool_wait(handles)
```

`wait_agent(handle)` from `std/agent/workers` recognises pool task
handles transparently, so user code that already waits on worker
handles does not need to learn a second waiter API.

## Queue strategies

`std/lifecycle/pool` exports four strategy factories. Pass the result
as `pool_create({queue: <strategy>})`:

| Function | Behavior |
|---|---|
| `fifo()` | Dequeue oldest queued task first. Ignores submit priority. |
| `priority()` | Dequeue highest submit-time `priority` first; equal priorities stay FIFO. Default. |
| `lifo()` | Dequeue newest queued task first. |
| `fair_round_robin(key = "key")` | Partition queued tasks by the submit option named by `key`, then rotate across distinct values while preserving FIFO inside each partition. |

```harn,ignore
import { QueueStrategy, pool_create } from "std/lifecycle/pool"

let queue = QueueStrategy()
let pool = pool_create({
  name: "tenant-work",
  max_concurrent: 4,
  queue: queue.fair_round_robin("tenant_id"),
})
```

`QueueStrategy()` is just sugar — it returns
`{fifo, priority, lifo, fair_round_robin}` for callers that prefer
dotted access after a single import.

### Fair round-robin partitioning

`fair_round_robin(key_field)` reads `options.<key_field>` on every
submit. Submits that omit the field share one "default" partition so
they do not starve. The runtime walks distinct partitions in stable
order, dequeuing one task per visit, until every queue is drained.

Two tenants with 100 tasks each interleave 1-for-1: tenant A's task 1,
tenant B's task 1, tenant A's task 2, tenant B's task 2, and so on. A
third tenant joining mid-drain slides into the rotation at its next
turn.

## Backpressure

`std/lifecycle/pool` exports three backpressure factories plus the
`Backpressure()` namespace helper:

| Function | Behavior |
|---|---|
| `backpressure_queue(max_depth, on_full = "block_submitter")` | Bounds queued tasks at `max_depth`. `on_full` is one of `"block_submitter"`, `"drop_oldest"`, `"drop_newest"`, or `"fail_submitter"`. |
| `fail_fast()` | Rejects any submit that cannot start immediately. No queue is retained. |
| `ring_buffer(capacity)` | Retains the newest `capacity` queued tasks by evicting the oldest queued task on overflow. |

```harn,ignore
import { Backpressure, pool_create } from "std/lifecycle/pool"

let bp = Backpressure()
let pool = pool_create({
  name: "webhook-intake",
  max_concurrent: 10,
  backpressure: bp.ring_buffer(100),
})
```

`on_full` policies:

| Policy | Effect |
|---|---|
| `"block_submitter"` | The submitting fiber parks until a slot is free. Default for `backpressure_queue`. |
| `"drop_oldest"` | Pop the oldest queued task as a `rejected` handle and accept the new submit. Emits `pool_drop` audit. |
| `"drop_newest"` | Reject the new submit as a `rejected` handle. Emits `pool_drop` audit. |
| `"fail_submitter"` | Raise `HARN-POL-001` at the submit call site. The caller can catch and retry. |

`fail_fast()` raises `HARN-POL-002` synchronously when no worker slot
is free. `drop_oldest`, `drop_newest`, and `ring_buffer` are the
*silent* drop policies: the submit returns a handle whose terminal
snapshot has `status: "rejected"`, `rejection_reason`, and
`rejection_policy`, and a `pool_drop` audit lands on the
`lifecycle.pool.audit` EventLog topic.

## Scopes and durability

A pool's `scope` decides where its state lives.

| Scope | State | Survives | Notes |
|---|---|---|---|
| `"session"` | VM-scoped in-memory registry. | VM/session lifetime. | Default. Zero I/O. |
| `"pipeline"` | JSONL append-log under `.harn/pools/<pipeline_id>__<pool_name>.jsonl`. | Process restart, within one pipeline. | Terminal and stale task metadata reload on next `pool_create({scope: "pipeline", ...})`. |
| `"tenant"` | Host-managed registry. | Tenant lifetime, cross-pipeline. | Requires an embedding host with tenant pool routing. The in-process runtime rejects it. |
| `"org"` | Host-managed registry. | Org lifetime, cross-tenant. | Requires an embedding host with org pool routing. The in-process runtime rejects it. |

### Pipeline scope on reload

When a pipeline-scope pool reloads after a restart:

1. The JSONL log is replayed to reconstruct queued + in-flight tasks.
2. Any queued or running task is restored as a failed stale marker,
   because the in-memory closure body cannot be reconstructed after
   process death.
3. Tasks with `idempotency_key` short-circuit on resubmit: the second
   `pool.submit(closure, {idempotency_key: "..."})` returns the
   previously recorded terminal snapshot instead of running again.
   Submit fresh work with a new key when you want a retry attempt.

The log is compacted opportunistically: when reload sees
`max_concurrent + |queue| + |terminal-since-compaction|` exceed an
internal threshold, the next `pool_create` rewrites the log with only
live state.

`pool_simulate_restart()` drops the in-process registry without
touching disk, so pipeline-scope pools can be exercised under
conformance tests by re-calling `pool_create(...)` and asserting the
stale records rehydrate.

## Idempotency

`options.idempotency_key` on `pool.submit` makes the submission
idempotent. Two submits with the same `(pool_id, idempotency_key)`
return the *same* task handle:

```harn,ignore
let first = pool.submit({ -> review(pr) }, {idempotency_key: "review-pr-1984"})
let second = pool.submit({ -> review(pr) }, {idempotency_key: "review-pr-1984"})
log(first.id == second.id)   // true
```

This is load-bearing for pipeline-scope pools: a worker that crashed
mid-submit can re-enter with the same key and pick up the previously
recorded outcome instead of double-running the closure.

## Trigger composition: `SpawnToPool`

A `channel.emit` trigger (or any trigger source) can route matched
events into a named pool instead of spawning a fresh worker per event.
Use the `SpawnToPool` handler variant from `std/triggers`:

```harn,ignore
import { trigger_register, SpawnToPool } from "std/triggers"
import { pool_create, fair_round_robin } from "std/lifecycle/pool"

let pool = pool_create({
  name: "webhook-work",
  max_concurrent: 10,
  queue: fair_round_robin("source"),
})

trigger_register({
  id: "webhook-router",
  kind: "channel.emit",
  provider: "channel",
  match: {events: ["channel:webhook.received"]},
  handler: SpawnToPool({
    pool: "webhook-work",
    priority_from: "provider_payload.payload.urgency",
    key_from: "provider_payload.payload.source",
    task_factory: { event -> { -> handle_webhook(event) } },
  }),
})
```

`SpawnToPool` options:

| Option | Notes |
|---|---|
| `pool` | Name (or id) of the pool to submit into. The pool must already exist; missing pools land in `trigger.dlq`. |
| `priority_from` | Dotted JSON path into the trigger event. Resolved to an int and passed as `options.priority`. Missing path falls back to `0`. |
| `key_from` | Dotted JSON path resolved to a string and passed as `options.key` (and as the partition value for `fair_round_robin`). Missing path falls back to the default partition. |
| `task_factory` | Closure `event -> closure`. Builds the per-event zero-arg closure the pool submits. |

Each dispatched event becomes one `pool.submit` call. The trigger
dispatcher records the resulting task id on the match receipt so
replay can verify the same event mapped to the same pool task across
runs.

## Inspection

| Surface | Purpose |
|---|---|
| `pool.size()` | Live count of active + queued tasks. Excludes terminal-state tasks. |
| `pool.snapshot()` | Full dict: `active`, `queued`, `completed`, `failed`, `rejected`, `blocked_submitters`, `total`, selected `queue`, selected `backpressure`, `scope`, `scope_id`, `stale_after_ms`, the per-task list, and the original `config` so dashboards can show "what was configured". |
| `pool_get(name_or_id)` | Lookup by name. Returns `nil` when missing. |
| `pool_list()` | Every pool registered on the current runtime. |
| `harness.unsettled_state().pool_pending_tasks` | Pipeline-lifecycle integration: lists tasks blocking pipeline finalization. See [pipeline lifecycle](./stdlib/lifecycle.md). |

## Observability

Pools produce one observable stream end-to-end:

| Topic | Purpose | Records |
|---|---|---|
| `lifecycle.pool.audit` | Durable audit. Receipts for submit, dequeue, and drop. | `pool_submit`, `pool_dequeue`, `pool_drop` |

OTel spans wrap every submit (`pool.submit <pool_name>`) and every
dequeue (`pool.dequeue <pool_name>`). The dequeue span links back
to the submit span so traces across the
async boundary stay stitched even when the queue holds tasks for
minutes.

The pool task snapshot carries `submitted_by` (the caller's
`workflow_id`, `agent_session_id`, or `worker_id`, falling back to
`"user"`) so audit consumers can attribute work back to its origin.

## Diagnostic codes

| Code | When |
|---|---|
| `HARN-POL-001` | A `backpressure_queue(..., "fail_submitter")` pool was full at submit time. |
| `HARN-POL-002` | A `fail_fast()` pool had no immediate capacity at submit time. |

Drop policies (`drop_oldest`, `drop_newest`, `ring_buffer`) do **not**
raise — they emit a `pool_drop` audit and return a `rejected` task
handle. Errors raised inside the closure surface as
`status: "failed"` with the error message on the task snapshot.

## Parallel sub-agent fan-out: `agent_fanout`

A pool is the right tool when *many independent submitters* must share *one*
budget, or when work must survive a restart. For the common, simpler case —
**one orchestrator that wants to map a list of independent units onto concurrent
child agents and collect their results** — `std/agent/workers` exposes
`agent_fanout`, a thin composition over the worker primitives you already have
(`sub_agent_run({background: true})` + `wait_agent`):

```harn,ignore
import { agent_fanout } from "std/agent/workers"

let results = agent_fanout(
  [
    {task: "Port src/a to the new API", options: child_opts_a, label: "a"},
    {task: "Port src/b to the new API", options: child_opts_b, label: "b"},
    {task: "Port src/c to the new API", options: child_opts_c, label: "c"},
  ],
  {max_parallel: 8},
)

for r in results {
  log("${r.label}: ${r.ok ? "ok" : "failed"}")
}
```

Each request is launched as a background `sub_agent_run`, so a whole wave's LLM
turns **overlap in wall-clock time** — the orchestrator's `wait_agent` await is
where the siblings make progress. `max_parallel` bounds how many run at once
(default 8); units beyond it run in later waves, so a large fan-out cannot
exhaust the provider rate budget. Results come back in **input order**
regardless of completion order, each normalised to
`{label, index, status, ok, result, error}`.

`agent_fanout` owns only the concurrency, the wave bound, the join, and result
normalisation. The caller owns everything that makes a child a child — its tool
surface, capability policy, model, and system prompt — by setting each request's
`options` (forwarded verbatim to `sub_agent_run`). That keeps product-specific
delegation policy in the harness while the orchestration mechanics stay general.

### `agent_fanout` vs pools

| | `agent_fanout` | Pool (`pool.submit`) |
|---|---|---|
| Concurrency | Cooperative on the caller's `LocalSet` (IO-overlap) | OS-thread parallel (`tokio::spawn`) |
| Budget | One wave bound per call | Shared named budget across submitters |
| Queue strategy / fairness | FIFO waves | Pluggable (`fifo`/`priority`/`fair_round_robin`) |
| Durability | In-process | `scope: "pipeline"` survives restart |
| Best for | One orchestrator fanning out independent child agents | Many submitters sharing a rate-limited tier |

Reach for `agent_fanout` first; graduate to a pool when you need a shared budget,
fairness, or restart safety.

## Design rationale

Pools close a gap between Harn's two existing concurrency tools.
`parallel each ... with { max_concurrent: N }` bounds one call site;
spawned workers carry one budget per spawn. Neither lets *many
independent submitters* share *one* budget across a pipeline or
process, which is exactly what rate-limiting an LLM tier or fairly
draining a multi-tenant queue requires.

| System | Per-callsite bound | Shared budget across submitters | Fair multi-tenant queue | Durable across restart |
|---|---|---|---|---|
| Temporal `ChildWorkflowOptions` | Limited (per workflow) | No first-class primitive | No | Yes |
| Inngest `concurrency` keys | Yes | Yes (per key) | Yes (`concurrency.key`) | Yes |
| Restate keyed services | Yes | Yes (per key) | Yes (per key) | Yes |
| **Harn pools** | `parallel each` | **`pool_create + pool.submit`** | **`fair_round_robin(key)`** | **`scope: "pipeline"`** |

The wager is that agent orchestration needs all four — local bounds,
shared bounds, fair partitioning, and restart-safe queues — and that
hosting them in one primitive (named pools with pluggable queue +
backpressure descriptors) keeps the trust boundary (Harn owns the
queue and audit; hosts own remote execution) and the replay model
(`lifecycle.pool.audit` is the byte-comparable spec) intact.

Cross-references:

- [Pool cookbook](./cookbooks/pools.md) — four end-to-end recipes
  (rate-limited webhook processor, GPU-routed inference, cross-customer
  fairness, burst absorber).
- [Pool stdlib](./stdlib/lifecycle-pool.md) — exhaustive builtin and
  option reference.
- [Triggers](./triggers.md) — the general trigger surface that
  `SpawnToPool` plugs into.
- [Pipeline lifecycle presets](./stdlib/lifecycle.md) — pool tasks
  surface in `harness.unsettled_state().pool_pending_tasks`.
- Issue [#1883](https://github.com/burin-labs/harn/issues/1883) — the
  epic this primitive lands under.
