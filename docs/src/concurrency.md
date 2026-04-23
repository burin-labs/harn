# Concurrency

Harn has built-in concurrency primitives that don't require callbacks, promises, or async/await boilerplate.

## spawn and await

Launch background tasks and collect results:

```harn
let handle = spawn {
  sleep(1s)
  "done"
}

let result = await(handle)  // blocks until complete
println(result)                 // "done"
```

Cancel a task before it finishes:

```harn
let handle = spawn { sleep(10s) }
cancel(handle)
```

Each spawned task runs in an isolated interpreter instance.

## parallel

Run N tasks concurrently and collect results in order:

```harn
let results = parallel(5) { i ->
  i * 10
}
// [0, 10, 20, 30, 40]
```

The variable `i` is the zero-based task index. Results are always returned
in index order regardless of completion order.

## parallel each

Map over a collection concurrently:

```harn
let files = ["a.txt", "b.txt", "c.txt"]

let contents = parallel each files { file ->
  read_file(file)
}
```

Results preserve the original list order.

## parallel settle

Like `parallel each`, but never throws. Instead, it collects both
successes and failures into a result object:

```harn
let items = [1, 2, 3]
let outcome = parallel settle items { item ->
  if item == 2 {
    throw "boom"
  }
  item * 10
}

println(outcome.succeeded)  // 2
println(outcome.failed)     // 1

for r in outcome.results {
  if is_ok(r) {
    println(unwrap(r))
  } else {
    println(unwrap_err(r))
  }
}
```

The return value is a dict with:

| Field | Type | Description |
|---|---|---|
| `results` | list | List of `Result` values (one per item), in order |
| `succeeded` | int | Number of `Ok` results |
| `failed` | int | Number of `Err` results |

This is useful when you want to process all items and handle failures
after the fact, rather than aborting on the first error.

## retry

Automatically retry a block that might fail:

```harn
retry 3 {
  http_get("https://flaky-api.example.com/data")
}
```

Executes the body up to N times. If the body succeeds, returns immediately.
If all attempts fail, returns `nil`. Note that `return` statements inside
`retry` propagate out (they are not retried).

## Channels

Message-passing between concurrent tasks:

```harn
let ch = channel("events")
send(ch, {event: "start", timestamp: timestamp()})
let msg = receive(ch)
```

## Channel iteration

You can iterate over a channel with a `for` loop. The loop receives
messages one at a time and exits when the channel is closed and fully
drained:

```harn
let ch = channel("stream")

spawn {
  send(ch, "chunk 1")
  send(ch, "chunk 2")
  close_channel(ch)
}

for chunk in ch {
  println(chunk)
}
// prints "chunk 1" then "chunk 2", then the loop ends
```

This is especially useful with `llm_stream`, which returns a channel
of response chunks:

```harn
let stream = llm_stream("Tell me a story", "You are a storyteller")
for chunk in stream {
  print(chunk)
}
```

Use `try_receive(ch)` for non-blocking reads -- it returns `nil`
immediately if no message is available. Use `close_channel(ch)` to
signal that no more messages will be sent.

## Scoped shared state

Normal values are copied into child VMs. Use shared cells or maps only when
tasks need to coordinate on the same mutable state.

```harn
let budget = shared_cell({scope: "task_group", key: "tokens", initial: 0})

parallel 10 { i ->
  var updated = false
  while !updated {
    let snap = shared_snapshot(budget)
    updated = shared_cas(budget, snap, snap.value + 1)
  }
}

println(shared_get(budget)) // 10
```

Scopes are explicit:

| Scope | Meaning |
|---|---|
| `task` | Current logical task only |
| `task_group` | Siblings from one `parallel` operation, or the root task when no group is active |
| `workflow_run` | Current workflow run when available |
| `agent_session` | Current agent session when available |
| `tenant` | Current tenant id, or `tenant_id` supplied in options |
| `process` | This VM process |

Durable and external state remain explicit: use `store_*` or `agent_state_*`
for EventLog/file-backed state, and host or connector builtins for external
stores.

Cells support last-write-wins `shared_set(cell, value)`, versioned reads with
`shared_snapshot(cell)`, and atomic compare-and-swap with
`shared_cas(cell, expected_or_snapshot, value)`. Passing a snapshot to CAS
detects stale reads when another writer has changed the cell since the read.

Maps provide the same conflict model per map:
`shared_map_get`, `shared_map_set`, `shared_map_delete`,
`shared_map_snapshot`, `shared_map_cas`, and `shared_map_entries`.
`shared_metrics(handle)` reports read, write, CAS success/failure, and stale
read counters.

Use the named synchronization primitives when an update needs a larger
critical section:

```harn
let memo = shared_map({scope: "workflow_run", key: "memo"})
let permit = sync_mutex_acquire("memo:customer-42", 250ms)
guard permit != nil else { throw "memo lock timeout" }
try {
  shared_map_set(memo, "customer-42", compute_customer_summary())
} finally {
  sync_release(permit)
}
```

## Actor mailboxes

Mailboxes are named inboxes for actor-style communication between tasks and
long-lived workers. They use explicit messages instead of transcript mutation.

```harn
let inbox = mailbox_open({scope: "task_group", name: "reviewer", capacity: 32})

spawn {
  mailbox_send("reviewer", {kind: "work", path: "src/main.rs"})
}

let msg = mailbox_receive(inbox)
println(msg.kind)
```

`mailbox_send(target, value)` returns `false` when the target does not exist or
has been closed. `mailbox_try_receive(target)` is non-blocking.
`mailbox_receive(target)` blocks until a message arrives, the mailbox closes,
or the task is cancelled. `mailbox_metrics(target)` reports depth, capacity,
sent, received, failed send, and closed status.

Examples:

```harn
// Connector token refresh: only one task refreshes the token.
let tokens = shared_map({scope: "tenant", tenant_id: "acme", key: "connector_tokens"})
let lock = sync_mutex_acquire("token:acme:slack", 2s)
guard lock != nil else { throw "token refresh busy" }
try { shared_map_set(tokens, "slack", refresh_slack_token()) } finally { sync_release(lock) }

// Workflow memoization: cache pure stage output for this run.
let memo = shared_map({scope: "workflow_run", key: "stage_memo"})
let cached = shared_map_get(memo, "normalize", nil)
if cached == nil {
  shared_map_set(memo, "normalize", normalize(input))
}

// Multi-agent scratchpad: parent and workers exchange notes explicitly.
let scratch = shared_map({scope: "agent_session", key: "scratchpad"})
shared_map_set(scratch, "hypothesis", "retry with smaller batch")

// Shared budget counter: CAS avoids lost updates.
let spent = shared_cell({scope: "task_group", key: "budget_usd_micros", initial: 0})
var ok = false
while !ok {
  let snap = shared_snapshot(spent)
  ok = shared_cas(spent, snap, snap.value + 1250)
}
```

## Atomics

Thread-safe counters:

```harn
let counter = atomic(0)
println(atomic_get(counter))         // 0

let c2 = atomic_add(counter, 5)
println(atomic_get(c2))              // 5

let c3 = atomic_set(c2, 100)
println(atomic_get(c3))              // 100
```

Atomic operations return new atomic values (they don't mutate in place).

## Mutex

`mutex { ... }` is a process-local, fair critical section inherited by
`spawn` and `parallel` child VMs. It uses the default mutex key
`"__default__"` and releases automatically when the lexical scope exits,
including `throw`, `return`, `break`, and caught runtime errors.

```harn
mutex {
  // only one task executes this block at a time
  var count = count + 1
}
```

Use the named primitives when a workflow needs separate keys, timeouts,
or observable permits:

```harn
fn update_index() { nil }

let permit = sync_mutex_acquire("repo:index", 500ms)
guard permit != nil else { throw "timed out waiting for repo index" }
try {
  update_index()
} finally {
  sync_release(permit)
}
```

## Synchronization Taxonomy

Harn synchronization is intentionally higher-level than OS locks:

| Primitive | Scope | Fairness | Timeout/cancel | Use |
|---|---|---|---|---|
| `mutex { ... }` | process-local default key | FIFO | cancellable | Small critical-section updates |
| `sync_mutex_acquire(key, timeout?)` | process-local named key | FIFO | returns `nil` on timeout, throws on cancellation | Named critical sections |
| `sync_semaphore_acquire(key, capacity, permits?, timeout?)` | process-local named key | FIFO | returns `nil` on timeout, throws on cancellation | Bounded connector or model work |
| `sync_gate_acquire(key, limit, timeout?)` | process-local named key | FIFO | returns `nil` on timeout, throws on cancellation | Fair runner admission |

All permits are parking primitives, not spinlocks. A permit returned by
`sync_*_acquire` must be passed to `sync_release(permit)`. Releasing twice
returns `false`; the first release returns `true`.

`sync_metrics(kind?, key?)` reports wait/held counters for matching
primitives:

```harn
let m = sync_metrics("gate", "workflow-runner")
println(m?.acquisition_count)
println(m?.timeout_count)
println(m?.current_queue_depth)
```

Metrics include `acquisition_count`, `timeout_count`,
`cancellation_count`, `release_count`, `current_held`,
`current_queue_depth`, `max_queue_depth`, `total_wait_ms`, and
`total_held_ms`.

Examples:

```harn
fn poll_connector() { nil }
fn write_shared_state() { nil }

// Connector polling: cap concurrent calls against one provider.
let permit = sync_semaphore_acquire("connector:notion", 4, 1, 2s)
guard permit != nil else { throw "connector poll saturated" }
try { poll_connector() } finally { sync_release(permit) }

// Fair workflow runner admission.
let slot = sync_gate_acquire("workflow-runner", 8, 5s)
guard slot != nil else { throw "runner queue timed out" }
try { workflow_execute("task", {}, [], {}) } finally { sync_release(slot) }

// Critical-section update.
let lock = sync_mutex_acquire("state:account-42", 250ms)
guard lock != nil else { throw "state lock timeout" }
try { write_shared_state() } finally { sync_release(lock) }
```

## Deadline

Set a timeout on a block of work:

```harn
deadline 30s {
  // must complete within 30 seconds
  agent_loop(task, system, {persistent: true})
}
```

## Defer

Register cleanup code that runs when the enclosing scope exits, whether
by normal return or by a thrown error:

```harn
fn open(path) { return path }
fn close(f) { log("closed ${f}") }

let f = open("data.txt")
defer { close(f) }
// ... use f ...
// close(f) runs automatically on scope exit
```

Multiple `defer` blocks execute in LIFO (last-registered, first-executed)
order, similar to Go's `defer`.

## Capping in-flight work with `max_concurrent`

`parallel each`, `parallel settle`, and `parallel N` all accept an
optional `with { max_concurrent: N }` clause that caps how many
workers are in flight at once. Tasks past the cap wait until a slot
frees up — fan-out stays bounded while the total work is unchanged.

```harn
// Without a cap: all 200 requests hit the server at once.
let results = parallel settle paths { p -> llm_call(p, nil, opts) }

// With max_concurrent=8: at most 8 in-flight calls at any moment.
let results = parallel settle paths with { max_concurrent: 8 } { p ->
  llm_call(p, nil, opts)
}
```

`max_concurrent: 0` (or a missing `with` clause) means unlimited.
Negative values are treated as unlimited. The cap applies to every
parallel mode, including the count form:

```harn
fn process(i) { log(i) }

parallel 100 with { max_concurrent: 4 } { i ->
  process(i)
}
```

## Rate limiting LLM providers

`max_concurrent` bounds *simultaneous* in-flight tasks on the caller's
side. A provider can additionally be rate-limited at the throughput
layer (requests per minute). The RPM limiter is a sliding-window
budget enforced before each `llm_call` / `llm_completion` — requests
past the budget wait for the window to free up rather than error.

Configure RPM per provider via:

- `rpm: 600` in the provider's entry in `providers.toml` / `harn.toml`.
- `HARN_RATE_LIMIT_<PROVIDER>=600` environment variable
  (e.g. `HARN_RATE_LIMIT_TOGETHER=600`, `HARN_RATE_LIMIT_LOCAL=60`).
  Env overrides config.
- `llm_rate_limit("provider", 600)` at runtime from a pipeline.

The two controls compose: `max_concurrent` prevents bursts from
saturating the server; RPM shapes sustained throughput. When batching
hundreds of LLM calls against a local single-GPU server, both are
worth setting — otherwise the RPM budget can be spent in a 2-second
burst that overwhelms the queue and drops requests.
