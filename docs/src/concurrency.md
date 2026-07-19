# Concurrency

Harn concurrency is structured around child tasks. A child task runs the block
you give to `spawn` or `parallel` in its own interpreter instance, while the
parent keeps a handle, result list, stream, or settlement record that joins the
work back together.

The practical model is:

- normal values are copied into the child task when it starts
- channels, shared cells/maps, mailboxes, and sync permits are explicit shared
  handles
- cancellation and deadlines flow from the parent into children
- results come back through `await`, `parallel`, `parallel each`,
  `parallel settle`, or streams

That gives Harn the same shape on the page as the work you are trying to run:
fan out, wait, collect, and clean up.

## spawn and await

Launch background tasks and collect results:

```harn
const handle = spawn {
  sleep(1s)
  "done"
}

const result = await(handle)  // blocks until complete
log(result)                 // "done"
```

Cancel a task before it finishes:

```harn
const handle = spawn { sleep(10s) }
cancel(handle)
```

Each spawned task runs in an isolated interpreter instance. In runtime and ADR
pages you may see this called a **child VM**; in day-to-day Harn code, read that
as "the child task's interpreter."

A bare `spawn` is detached: if you never `await` its handle, the task may
outlive the code that launched it and any error it raises is lost. Reach for a
`scope { }` nursery (below) whenever you want those guarantees back.

## Structured concurrency: `scope`

A `scope { }` block is a structured-concurrency *nursery*: every task spawned
while it is open is **joined when the block exits**. You cannot accidentally
leak a task past its scope, and a task's error is never silently swallowed —
the first failing task's error propagates out of the block (so you can `try`
it) after its siblings are cancelled.

```harn
fn fetch(url) {
  log(url)
}

scope {
  spawn {
    fetch("https://a")
  }
  spawn {
    fetch("https://b")
  }
}
// Both fetches have finished here — the block did not exit until they did.
```

- **Joins on exit.** Reaching the end of the block waits for every task spawned
  inside it.
- **Errors propagate.** If a task throws, the block re-raises that error after
  cancelling the remaining siblings; wrap the `scope { }` in `try { }` to handle
  it.
- **No leaks on early exit.** A `throw`, `return`, or `break` out of the block
  cancels the tasks still bound to it rather than detaching them.
- **`await` opts a task out.** Explicitly `await`-ing a handle inside the scope
  removes it from the nursery, so it is not joined or cancelled a second time.
- **Contextual keyword.** `scope` is only special as a statement-leading
  `scope { }`; identifiers, dict keys, and properties named `scope` keep working.

## parallel

Run N tasks concurrently and collect results in order:

```harn
const results = parallel(5) { i ->
  i * 10
}
// [0, 10, 20, 30, 40]
```

The variable `i` is the zero-based task index. Results are always returned
in index order regardless of completion order.

`parallel` and `parallel each` are fail-fast: the first branch that
throws cancels all in-flight siblings (queued branches never start) and
its error propagates out of the construct. When several branches have
already failed by the time the cancellation lands, the lowest-index
branch's error is the one reported. If you need every branch to run
regardless of failures, use `parallel settle`.

## parallel each

Map over a collection concurrently:

```harn
const files = ["a.txt", "b.txt", "c.txt"]

const contents = parallel each files { file ->
  read_file(file)
}
```

Results preserve the original list order.

Use `as stream` when callers need completion-order progress instead of
an eager list:

```harn
const results = parallel each [30, 5, 10] with { max_concurrent: 2 } { ms ->
  sleep(ms)
  ms
} as stream

for result in results {
  log(result)
}
```

`parallel_race(items, callable, options?)` returns the first successful
plain value or `Result.Ok` payload, cancels the remaining tasks, and
throws an aggregate error if every task throws or returns `Result.Err`.

## parallel settle

Like `parallel each`, but never throws and never cancels — every branch
runs to completion regardless of sibling failures. Instead, it collects
both successes and failures into a result object:

```harn
const items = [1, 2, 3]
const outcome = parallel settle items { item ->
  if item == 2 {
    throw "boom"
  }
  item * 10
}

log(outcome.succeeded)  // 2
log(outcome.failed)     // 1

for r in outcome.results {
  if is_ok(r) {
    log(unwrap(r))
  } else {
    log(unwrap_err(r))
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
const ch = channel("events")
send(ch, {event: "start", timestamp: timestamp()})
const msg = receive(ch)
```

## Channel iteration

You can iterate over a channel with a `for` loop. The loop receives
messages one at a time and exits when the channel is closed and fully
drained:

```harn
const ch = channel("stream")

spawn {
  send(ch, "chunk 1")
  send(ch, "chunk 2")
  close_channel(ch)
}

for chunk in ch {
  log(chunk)
}
// prints "chunk 1" then "chunk 2", then the loop ends
```

This is especially useful with `llm_stream`, which returns a channel
of response chunks:

```harn
const stream = llm_stream("Tell me a story", "You are a storyteller")
for chunk in stream {
  log(chunk)
}
```

Use `try_receive(ch)` for non-blocking reads -- it returns `nil`
immediately if no message is available. Use `close_channel(ch)` to
signal that no more messages will be sent. After close, direct `send(ch, value)`
raises `ChannelClosed`. Direct `receive(ch)` still drains buffered messages, then
raises `ChannelClosed` once the channel is closed and empty. Channel iteration
and `try_receive(ch)` stay probe/drain-oriented: iteration exits after the drain,
and `try_receive(ch)` returns `nil` when no value is immediately available.

When every active task is blocked on channel operations that cannot match
another sender or receiver, the runtime raises `HARN-ORC-012` instead of
letting the run hang forever. Channel waits inside `deadline { ... }` are
interrupted by the deadline, and timeout-based selects still resolve through
their timeout path.

For dynamic fan-in, `channel_select([ch1, ch2, ...], timeout?)` mirrors the
`select { ... }` statement but takes a runtime list of channels and returns the
same `{index, value, channel}` dict.

## Scoped shared state

Normal values are copied into child tasks. Use shared cells or maps only when
tasks need to coordinate on the same mutable state.

The multithreaded runtime keeps child interpreters isolated, and the shared
state primitives are the explicit bridge between them. Shared cells/maps,
mailboxes, mutexes, rwlocks, semaphores, and gates are thread-safe handles
inherited by `spawn` and `parallel` children. If you do not pass one of those
handles, the child sees its own copy of the value it captured at start.

```harn
const budget = shared_cell({scope: "task_group", key: "tokens", initial: 0})

parallel 10 { i ->
  let updated = false
  while !updated {
    const snap = shared_snapshot(budget)
    updated = shared_cas(budget, snap, snap.value + 1)
  }
}

log(shared_get(budget)) // 10
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

```harn,ignore
const memo = shared_map({scope: "workflow_run", key: "memo"})
const permit = sync_mutex_acquire("memo:customer-42", 250ms)
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
const inbox = mailbox_open({scope: "task_group", name: "reviewer", capacity: 32})

spawn {
  mailbox_send("reviewer", {kind: "work", path: "src/main.rs"})
}

const msg = mailbox_receive(inbox)
log(msg.kind)
```

`mailbox_send(target, value)` returns `false` when the target does not exist or
has been closed. `mailbox_try_receive(target)` is non-blocking.
`mailbox_receive(target)` blocks until a message arrives, the mailbox closes,
or the task is cancelled. `mailbox_metrics(target)` reports depth, capacity,
sent, received, failed send, and closed status.

Examples:

```harn,ignore
// Connector token refresh: only one task refreshes the token.
const tokens = shared_map({scope: "tenant", tenant_id: "acme", key: "connector_tokens"})
const lock = sync_mutex_acquire("token:acme:slack", 2s)
guard lock != nil else { throw "token refresh busy" }
try { shared_map_set(tokens, "slack", refresh_slack_token()) } finally { sync_release(lock) }

// Workflow memoization: cache pure stage output for this run.
const memo = shared_map({scope: "workflow_run", key: "stage_memo"})
const cached = shared_map_get(memo, "normalize", nil)
if cached == nil {
  shared_map_set(memo, "normalize", normalize(input))
}

// Multi-agent scratchpad: parent and workers exchange notes explicitly.
const scratch = shared_map({scope: "agent_session", key: "scratchpad"})
shared_map_set(scratch, "hypothesis", "retry with smaller batch")

// Shared budget counter: CAS avoids lost updates.
const spent = shared_cell({scope: "task_group", key: "budget_usd_micros", initial: 0})
let ok = false
while !ok {
  const snap = shared_snapshot(spent)
  ok = shared_cas(spent, snap, snap.value + 1250)
}
```

## Atomics

Thread-safe counters:

```harn
const counter = atomic(0)
log(atomic_get(counter))         // 0

const before_add = atomic_add(counter, 5)
log(before_add)                  // 0
log(atomic_get(counter))         // 5

const before_set = atomic_set(counter, 100)
log(before_set)                  // 5
log(atomic_get(counter))         // 100
```

Atomic updates mutate the handle and return the previous integer value.

## Mutex

`mutex { ... }` is a process-local, fair critical section inherited by
`spawn` and `parallel` child tasks. It releases automatically when the lexical
scope exits, including `throw`, `return`, `break`, and caught runtime errors.

A bare `mutex { ... }` keys on its own **lexical call-site**, so two distinct
`mutex {}` blocks are independent locks and do not serialize against each other.
To guard a shared resource across blocks, name it — `mutex(resource) { ... }`
keys on the resource expression's structural value, so every block naming the
same resource mutually excludes regardless of where it appears:

```harn
fn apply_charge(id) {
  log(id)
}

const account_id = "acct-42"

let count = 0

mutex {
  // only one task executes this particular block at a time
  count = count + 1
}

mutex(account_id) {
  // serializes only against other `mutex(account_id)` blocks for the same id
  apply_charge(account_id)
}
```

Re-acquiring the same key on a single task (e.g. `mutex(x) { mutex(x) { } }`)
raises `HARN-ORC-011` rather than deadlocking.

Use the named primitives when a workflow needs separate keys, timeouts,
or observable permits:

```harn
fn update_index() { nil }

const permit = sync_mutex_acquire("repo:index", 500ms)
guard permit != nil else { throw "timed out waiting for repo index" }
try {
  update_index()
} finally {
  sync_release(permit)
}
```

## Synchronization taxonomy

Harn synchronization is intentionally higher-level than OS locks:

| Primitive | Scope | Fairness | Timeout/cancel | Use |
|---|---|---|---|---|
| `mutex { ... }` | process-local, per-call-site key | FIFO | cancellable | Small critical-section updates |
| `mutex(resource) { ... }` | process-local, structural-value key | FIFO | cancellable | Guarding a named shared resource |
| `sync_mutex_acquire(key, timeout?)` | process-local named key | FIFO | returns `nil` on timeout, throws on cancellation | Named critical sections |
| `sync_rwlock_acquire(key, mode, timeout?)` | process-local named key | FIFO | returns `nil` on timeout, throws on cancellation | Shared readers / exclusive writers |
| `sync_semaphore_acquire(key, capacity, permits?, timeout?)` | process-local named key | FIFO | returns `nil` on timeout, throws on cancellation | Bounded connector or model work |
| `sync_gate_acquire(key, limit, timeout?)` | process-local named key | FIFO | returns `nil` on timeout, throws on cancellation | Fair runner admission |

All permits are parking primitives, not spinlocks. A permit returned by
`sync_*_acquire` is owned by the current scope and is released automatically
when that scope or frame exits, including `return` and `throw`. Pass the permit
to `sync_release(permit)` when you need an earlier release. Releasing twice
returns `false`; the first release returns `true`.

`sync_rwlock_acquire(key, "read", timeout?)` takes one shared permit, while
`sync_rwlock_acquire(key, "write", timeout?)` waits for exclusive access.

`sync_metrics(kind?, key?)` reports wait/held counters for matching
primitives:

```harn
const m = sync_metrics("gate", "workflow-runner")
log(m?.acquisition_count)
log(m?.timeout_count)
log(m?.current_queue_depth)
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
const permit = sync_semaphore_acquire("connector:notion", 4, 1, 2s)
guard permit != nil else { throw "connector poll saturated" }
try { poll_connector() } finally { sync_release(permit) }

// Fair workflow runner admission.
const slot = sync_gate_acquire("workflow-runner", 8, 5s)
guard slot != nil else { throw "runner queue timed out" }
try { workflow_execute("task", {}, [], {}) } finally { sync_release(slot) }

// Critical-section update.
const lock = sync_mutex_acquire("state:account-42", 250ms)
guard lock != nil else { throw "state lock timeout" }
try { write_shared_state() } finally { sync_release(lock) }
```

## Durable cross-process rate limiting

The `sync_*` primitives above are process-local permits. Use
`durable_rate_limit_acquire(options)` when multiple Harn processes, CLI
invocations, eval runners, or worker pools need to share one quota. The
primitive stores sliding-window reservations in SQLite, acquires all requested
buckets in one transaction, sleeps outside the transaction, and returns a
structured timeout instead of hanging forever when `timeout_ms` is exhausted.

By default the state lives at `.harn/rate-limits.sqlite` under the runtime
state root. Pass `state_path` when several repositories or runner fleets should
share a quota DB:

```harn
const gate = durable_rate_limit_acquire({
  state_path: ".harn/eval-rate-limits.sqlite",
  buckets: [
    {key: "provider:cerebras:rpm", limit: 5, units: 1, window_ms: 60s},
    {
      key: "model:cerebras:gpt-oss-120b:tpm",
      limit: 30000,
      units: 12000,
      window_ms: 60s,
    },
  ],
  timeout_ms: 2m,
})

guard gate.ok else {
  throw "rate-limit admission timed out after ${gate.waited_ms}ms"
}
```

Each bucket is `{key, limit, units?, window_ms?}`. Missing `units` means `1`;
missing `window_ms` means one minute. A reservation larger than the bucket limit
is admitted as one full-window charge so unusually large work can still run,
but the next reservation waits until the window clears. Duplicate bucket keys
are rejected because they would make the atomic reservation ambiguous.

For tests, combine `mock_time(...)` with `timeout_ms: 0` to assert admission or
timeout without real sleeps:

```harn
mock_time(1000)

const first = durable_rate_limit_acquire({key: "test", limit: 1, window_ms: 1s})
const second = durable_rate_limit_acquire({
  key: "test",
  limit: 1,
  window_ms: 1s,
  timeout_ms: 0,
})

log(first.ok)          // true
log(second.timed_out) // true
```

## Deadline

Set a timeout on a block of work:

```harn
deadline 30s {
  // must complete within 30 seconds
  agent_loop(task, system, {loop_until_done: true})
}
```

## Defer

Register cleanup code that runs when the enclosing lexical scope exits —
on normal fallthrough, on `return`, on `break` or `continue` out of a
loop, or on an uncaught throw:

```harn
fn open(path) { return path }
fn close(f) { log("closed ${f}") }

const f = open("data.txt")
defer { close(f) }
// ... use f ...
// close(f) runs automatically on scope exit
```

Multiple `defer` blocks in the same scope execute in LIFO
(last-registered, first-executed) order, similar to Zig's `defer`. Scope
is the innermost `{ ... }` (an `if` block, a loop body, a `try` block),
not the enclosing function — a `defer` inside a `for` body fires at the
end of each iteration.

## Owned handles and `drop()`

For stdlib handle types that need deterministic cleanup, annotate the
binding with `owned<T>` so the compiler registers an implicit
`defer { drop(<binding>) }` for you:

```harn
const ch: owned<channel> = channel("log", 64)
// equivalent to: const ch = channel("log", 64); defer { drop(ch) }
```

`drop()` is a builtin that dispatches on the runtime value tag — it
closes channels, releases sync permits, and falls through silently for
values that don't carry a close hook. The auto-drop fires alongside
user-written `defer` blocks in LIFO order, so:

```harn
const ch: owned<channel> = channel("log", 64)
defer { log("user defer first (LIFO)") }
// At scope exit: "user defer first (LIFO)" runs, then drop(ch).
```

`owned<T>` is transparent to the type system — an `owned<channel>` flows
into a parameter declared `channel` and vice versa. The marker only
influences scope-exit codegen and the `HARN-OWN-003` ownership-escape
lint, which fires when an `owned<T>` binding is returned from a function
whose return type is not also `owned<T>`. Widen the return type to
`owned<T>` to signal an explicit ownership transfer:

```harn
fn open_log() -> owned<channel> {
  const ch: owned<channel> = channel("log", 64)
  return ch        // ownership flows to the caller; auto-drop suppressed
}
```

## Capping in-flight work with `max_concurrent`

`parallel each`, `parallel settle`, and `parallel N` all accept an
optional `with { max_concurrent: N }` clause that caps how many
workers are in flight at once. Tasks past the cap wait until a slot
frees up — fan-out stays bounded while the total work is unchanged.

```harn
// Without a cap: all 200 requests hit the server at once.
const results = parallel settle paths { p -> llm_call(p, nil, opts) }

// With max_concurrent=8: at most 8 in-flight calls at any moment.
const results = parallel settle paths with { max_concurrent: 8 } { p ->
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
side. A provider or model route can additionally be rate-limited at
the throughput layer. Harn enforces catalog `rate_limits` metadata
before each `llm_call`: requests per minute (`rpm`), total tokens per
minute (`tpm`), split input/output token buckets (`input_tpm`,
`output_tpm`), and route concurrency. Requests past the budget wait
for the window to free up rather than error. RPM/TPM buckets are durable
across Harn processes by default, so parallel CLI, eval, and worker
processes share the same sliding-window admission state.

Configure provider limits via:

- `rpm: 600` in the provider's entry in `providers.toml` / `harn.toml`.
- `HARN_RATE_LIMIT_<PROVIDER>=600` environment variable
  (e.g. `HARN_RATE_LIMIT_TOGETHER=600`, `HARN_RATE_LIMIT_LOCAL=60`).
  This legacy form sets provider RPM.
- `HARN_RATE_LIMIT_<PROVIDER>_TPM=1000000` or
  `HARN_RATE_LIMIT_<PROVIDER>_RPM=1000` for richer quota overrides.
- `llm_rate_limit("provider", {rpm: 600, tpm: 1000000})` at runtime
  from a pipeline.

The controls compose: `max_concurrent` prevents caller-side bursts,
route `concurrency` caps in-flight provider calls, and RPM/TPM shapes
sustained throughput. Harn stores the default durable limiter under its
runtime state root; set `HARN_LLM_RATE_LIMIT_STATE_PATH` only when a
fleet runner needs an explicit shared SQLite path, and set
`HARN_LLM_RATE_LIMIT_DURABLE=0` only to bypass the durable layer in
constrained tests or embeddings. When batching hundreds of LLM calls
against a local single-GPU server or a low-tier hosted key, set both
concurrency and token/request throughput caps so a burst does not
exhaust the minute window and drop requests.

## Connector task groups

Connector code should treat `parallel ... with { max_concurrent: N }`,
`deadline`, and `cancel_graceful` as one structured unit: cap fan-out, put a
deadline around remote work, and let shutdown cancellation unwind outstanding
children. A deadline or host cancellation interrupts async waits and cancels
child tasks owned by the VM.

Paginated fetch with bounded page fan-out:

```harn
fn fetch_page(cursor) {
  connector_call("notion", "search", {cursor: cursor, page_size: 100})
}

fn collect_pages(cursors) {
  const outcome = deadline 30s {
    parallel settle cursors with { max_concurrent: 4 } { cursor ->
      fetch_page(cursor)
    }
  }

  let pages = []
  for result in outcome.results {
    if is_ok(result) {
      pages = pages.push(unwrap(result).items)
    } else {
      log("page fetch failed: ${unwrap_err(result)}")
    }
  }
  return pages
}
```

Stream shutdown with cooperative cancellation:

```harn
fn consume_stream(url) {
  const stream = sse_connect(url)
  defer { sse_close(stream) }

  while !is_cancelled() {
    const event = sse_receive(stream, 5000)
    if event == nil {
      continue
    }
    event_log_emit("connector.events", event.kind, event.payload)
  }
}

const reader = spawn { consume_stream(binding.stream_url) }
const shutdown = waitpoint_wait("connector.shutdown", {timeout: 1h})
if shutdown.status == "completed" {
  cancel_graceful(reader, 2s)
}
```

## Supervisor trees

Use `supervisor_start` when a workflow owns long-lived child loops that need
named lifecycle, restart policies, backoff, graceful drain, and introspection.
The child `task` is a closure called with `{supervisor_id, child_name,
child_kind, attempt, restart_count}`.

Connector stream:

```harn
const _streams = supervisor_start({
  name: "connector-streams",
  strategy: "one_for_one",
  children: [{
    name: "github-events",
    kind: "connector_stream",
    restart: {mode: "on_failure", max_restarts: 8, window_ms: 60000, backoff_ms: 250, factor: 2, jitter_ms: 100, circuit_open_ms: 300000},
    task: { _ctx ->
      const stream = sse_connect("https://example.invalid/events")
      while !is_cancelled() {
        const event = sse_receive(stream, 5000)
        if event != nil {
          event_log_emit("connector.github", event.kind, event.payload)
        }
      }
    },
  }],
})
```

Continuous persona:

```harn
const _persona = supervisor_start({
  name: "review-captain",
  strategy: "one_for_one",
  children: [{
    name: "inbox-loop",
    kind: "persona_loop",
    active_lease: "persona:review-captain",
    restart: {mode: "always", max_restarts: 10, window_ms: 300000, backoff_ms: 1000},
    task: { _ctx -> agent_loop("watch the review inbox", nil, {mode: "daemon"}) },
  }],
})
```

Actor/mailbox worker:

```harn
fn handle_patch_message(msg) {
  msg
}

const inbox = mailbox_open({scope: "task_group", name: "patcher", capacity: 32})

const _actor = supervisor_start({
  name: "patch-actors",
  strategy: "rest_for_one",
  children: [{
    name: "patcher",
    kind: "actor_mailbox",
    restart: {mode: "on_failure", max_restarts: 3, window_ms: 60000, backoff_ms: 100},
    task: { _ctx ->
      while !is_cancelled() {
        const msg = mailbox_receive(inbox)
        if msg != nil {
          handle_patch_message(msg)
        }
      }
    },
  }],
})
```

`supervisor_state(handle)` returns child status, restart counts, last errors,
wait reasons, active leases, next restart times, and aggregate metrics.
`supervisor_events(handle)` returns lifecycle events, and
`runtime_context().debug.supervisors` exposes supervisor state to tools.
`supervisor_wait(handle)` awaits the supervisor's terminal lifecycle transition
and returns its final state without polling.
`supervisor_stop(handle, timeout?)` requests cooperative cancellation and then
force-aborts children that do not drain before the timeout.
