## Concurrency

### Runtime context

`runtime_context()` returns the current logical runtime context as a dict.
`task_current()` is an alias. This is Harn's stable task/thread identity
surface; OS thread IDs are not exposed as the primary abstraction.

The stable fields are always present, with unavailable values represented as
`nil`: `task_id`, `parent_task_id`, `root_task_id`, `task_name`,
`task_group_id`, `scope_id`, `workflow_id`, `run_id`, `stage_id`, `worker_id`,
`agent_session_id`, `parent_agent_session_id`, `root_agent_session_id`,
`agent_name`, `trigger_id`, `trigger_event_id`, `binding_key`, `tenant_id`,
`provider`, `trace_id`, `span_id`, `scheduler_key`, `runner`,
`capacity_class`, `context_values`, `cancelled`, and `debug`.

`spawn`, `parallel`, `parallel each`, `parallel each ... as stream`, and
`parallel settle` create child logical tasks. A child task receives a deterministic `task_id`, its
`parent_task_id` is the creating task, and its `root_task_id` is inherited from
the root task. `parallel` siblings share a `task_group_id`.

Task-local values are managed with `runtime_context_values()`,
`runtime_context_get(key, default?)`, `runtime_context_set(key, value)`, and
`runtime_context_clear(key)`. Children inherit a snapshot of the parent's
task-local values. Later child writes do not mutate the parent, and later parent
writes do not affect already-created children.

### parallel

```harn
parallel(count) { i ->
  // body executed count times concurrently
}
```

Creates `count` concurrent tasks. Each task gets an isolated interpreter with a child
environment. The optional variable `i` is bound to the task index (0-based).
Returns a list of results in index order.

`parallel` is fail-fast: the first branch that throws cancels all
in-flight siblings and queued branches never start; the error then
propagates out of the construct. Cancelled siblings are still joined
before the construct returns, so no branch task outlives it. When more
than one branch has already failed by the time the cancellation lands,
the error from the lowest-index branch is the one propagated, so the
reported error is deterministic. If you need every branch to run
regardless of failures, use `parallel settle`.

### parallel each

```harn
parallel each list { item ->
  // body for each item
}
```

Maps over a list concurrently. Each task gets an isolated interpreter.
The variable is bound to the current list element.
Returns a list of results in the original order.

Errors are fail-fast with sibling cancellation, exactly like
`parallel`. Use `parallel settle` when every branch must run regardless
of failures.

### parallel each as stream

```harn
const results = parallel each list with { max_concurrent: 4 } { item ->
  // body for each item
} as stream

for result in results {
  log(result)
}
```

Maps over a list concurrently and returns `Stream<T>` instead of
materializing a result list. The stream emits each task result as soon as
that task completes, so output order is completion order rather than
source order. `with { max_concurrent: N }` is honored the same way as
eager `parallel each`. If a task throws, the error is raised when the
consumer pulls that stream item and remaining tasks are cancelled.

`parallel_race(items, callable, options?)` is the first-success helper
for this pattern. It returns the first plain value or `Result.Ok`
payload produced by `callable`, cancels remaining tasks, and throws an
aggregate error if every task throws or returns `Result.Err`.

### parallel settle

```harn
parallel settle list { item ->
  // body for each item
}
```

Like `parallel each`, but never throws and never cancels: a failing
branch does not affect its siblings, so every branch runs to
completion. `parallel settle` is the run-everything form — when you
need every branch to run regardless of failures, use it instead of the
fail-fast `parallel` / `parallel each`. It collects both successes and
failures into a result object with fields:

| Field | Type | Description |
|---|---|---|
| `results` | list | List of `Result` values (one per item), in order |
| `succeeded` | int | Number of `Ok` results |
| `failed` | int | Number of `Err` results |

All parallel forms accept `with { max_concurrent: N }` before the body:

```harn
fn fetch_page(cursor) { cursor }
const cursors = ["a", "b", "c"]
const pages = parallel settle cursors with { max_concurrent: 4 } { cursor ->
  fetch_page(cursor)
}
```

`max_concurrent` caps simultaneous in-flight child tasks. Missing,
zero, and negative values mean unlimited. Results are still returned in
source order.

### defer

```harn
defer {
  // cleanup body
}
```

Registers a block to run when the enclosing lexical scope exits — on normal
fallthrough, on `return`, on `break` / `continue` out of an enclosing loop,
or on an uncaught throw. Multiple `defer` blocks in the same scope execute
in LIFO (last-registered, first-executed) order, similar to Zig's `defer`.
The deferred block runs in the scope where it was declared.

Scope here is the innermost `{ ... }`, not the enclosing function. A `defer`
inside an `if` block fires when control leaves that `if`. A `defer` inside a
`for` body fires at the end of each iteration as well as on `break` /
`continue`.

```harn
fn open(path) { path }
fn close(f) { log("closing ${f}") }
const f = open("data.txt")
defer { close(f) }
// ... use f ...
// close(f) runs automatically on scope exit
```

### owned\<T\> and drop()

```harn
const ch: owned<channel> = channel("log", 64)
// implicit: defer { drop(ch) } registered at this `let`
```

`owned<T>` marks a binding as carrying sole ownership of a drop-able stdlib
handle. The compiler emits an implicit `defer { drop(<binding>) }` at the
`let` / `var` so the resource closes deterministically at lexical scope exit
— no manual `close_channel` / `mcp_disconnect` / etc. and no reliance on GC
finalisation. `drop()` is a builtin that dispatches on the runtime value tag:
channels close, sync permits release, future handle types add their own
hooks; unknown values are a silent no-op.

`owned<T>` is transparent for type compatibility: an `owned<channel>` value
flows into a parameter declared `channel`, and a `channel` value satisfies an
`owned<channel>` annotation. The marker only influences scope-exit codegen
and the ownership-escape lint below.

Returning an `owned<T>` binding by name from a function whose declared
return type is not also `owned<T>` defeats the auto-drop and fires
`HARN-OWN-003`. To **transfer ownership** to the caller, declare the
return type as `owned<T>`:

```harn
fn open_log() -> owned<channel> {
  const ch: owned<channel> = channel("log", 64)
  return ch                              // ownership transfers to caller
}
```

### spawn/await/cancel

```harn
const handle = spawn {
  // async body
}
const result = await(handle)
cancel(handle)
const outcome = cancel_graceful(handle, 2s)
```

`spawn` launches an async task and returns a `taskHandle`.
`await` (a built-in interpreter function, not a keyword) blocks until the task completes
and returns its result. `cancel` force-aborts the task. `cancel_graceful`
requests cooperative cancellation, waits up to the given duration, and then
force-aborts the task if it is still running. `is_cancelled()` reports whether
the current task has observed a cancellation request.

Cancellation is structured at the VM task boundary. Spawned tasks inherit the
parent's runtime context snapshot and cancellation token. The VM checks
cancellation between bytecode instructions and while awaiting interruptible
async operations, so CPU-bound Harn loops and blocked async calls both unwind.
When a VM exits, any un-awaited spawned tasks it owns are cancelled and aborted.

### std/signal

```harn
import "std/signal"

const registration = on_interrupt({ ->
  agent_session_close(session, {status: "interrupted"})
}, {signals: ["SIGINT", "SIGTERM"], once: true})

defer { off_interrupt(registration) }
```

`std/signal` exposes cooperative process interruption for scripts run by
`harn run`. `on_interrupt(handler, options?)` registers a callable for
`SIGINT`, `SIGTERM`, or `SIGHUP`; matching handlers run in LIFO order at the
next VM interrupt checkpoint. The default is `{signals: ["SIGINT"], once:
true}`. `graceful_timeout_ms` bounds handler execution at VM interrupt
checkpoints; an expired handler throws `kind:interrupted:handler_timeout`.
`off_interrupt(handle)` removes a handler, and `interrupted()` remains true
after the VM has observed an interrupt so tight loops can poll and return
through normal `defer` cleanup. `with_interrupt(handler, body, options?)`
registers a handler for the dynamic extent of `body` and always unregisters it
before returning or rethrowing.

If no matching handler is registered, process interruption falls back to normal
VM cancellation. A second process signal terminates `harn run` immediately so a
wedged handler cannot trap the operator.

### deadline

```harn
deadline 30s {
  // work must complete within 30 seconds
}
```

`deadline` applies a timeout scope to the block. If the deadline expires, Harn
throws `"Deadline exceeded"`, interrupts the active opcode or async wait, and
cancels child tasks owned by that scope's VM. The error can be caught with
`try`/`catch`.

### Synchronization

Harn synchronization primitives are workflow-level parking primitives, not
low-level OS locks or spinlocks. The initial scope is process-local: spawned and
parallel child VMs inherit the same synchronization runtime, while durable
EventLog-backed variants are reserved for explicit future primitives.

`mutex { ... }` acquires a fair process-local mutex keyed by the lexical
call-site of the block. Re-entering the same block serializes on the same key;
different bare `mutex { ... }` blocks do not contend with each other. The permit
is released when the block's scope exits, including `throw`, `return`, `break`,
`continue`, and caught runtime errors.

Named primitives return a permit value or `nil` on timeout:

```harn
const lock = sync_mutex_acquire("state:customer-42", 250ms)
const slot = sync_semaphore_acquire("connector:notion", 4, 1, 2s)
const gate = sync_gate_acquire("workflow-runner", 8, 5s)
```

- `sync_mutex_acquire(key?, timeout?)` acquires one permit from a named FIFO
  mutex. Omitting `key` uses `"__default__"`.
- `sync_semaphore_acquire(key, capacity, permits?, timeout?)` acquires a
  weighted permit from a named FIFO semaphore.
- `sync_gate_acquire(key, limit, timeout?)` acquires one fair-admission slot
  from a named FIFO gate.
- `sync_release(permit)` releases a named permit and returns `true` only for
  the first release.
- Permits returned by `sync_*_acquire` are also owned by the current scope or
  frame. They are released automatically on scope exit, `return`, and `throw`;
  explicit `sync_release` is for earlier release and remains idempotent.
- `sync_metrics(kind?, key?)` returns observability counters for matching
  primitives. A concrete `(kind, key)` returns a dict; partial or empty
  filters return a list.

Metrics include `acquisition_count`, `timeout_count`, `cancellation_count`,
`release_count`, `current_held`, `current_queue_depth`, `max_queue_depth`,
`total_wait_ms`, and `total_held_ms`.

Acquisition is cancellable: a graceful task cancellation while waiting throws
`kind:cancelled:VM cancelled by host`. Timeouts are deterministic and return
`nil` instead of throwing so authors can choose the fallback policy.

### Scoped shared state

Child tasks created by `spawn`, `parallel`, `parallel each`, and
`parallel settle` execute in isolated interpreter instances. Normal values are
copied into the child at creation time. Later assignment in one task does not
mutate another task's binding. Explicit handles are the shared-data boundary:
channels, atomics, synchronization permits/runtimes, shared cells/maps, and
mailboxes are shared by every task that receives or resolves the handle.

`agent_loop` does not share mutable transcript internals with tasks. A named
`session_id` shares durable transcript history through the agent session store.
`spawn_agent` workers have independent worker/session lineage; use explicit
mailboxes, shared state handles, `agent_state_*`, or host storage for data
exchange outside the transcript.

Named sessions may also carry a small `scratchpad` dict plus a monotonic
`scratchpad_version`. The scratchpad is live session-local working memory, not a
replayable transcript message. `agent_session_scratchpad(id)`,
`agent_session_set_scratchpad(id, scratchpad, opts?)`, and
`agent_session_clear_scratchpad(id, opts?)` are the direct state boundary.
Updates append compact `agent_scratchpad` transcript events that include
action, version, source, reason, counts, and caller metadata without copying the
full scratchpad into the event. Snapshots expose `scratchpad` and
`scratchpad_version`; final transcripts also mirror the current values under
`metadata.agent_scratchpad` and `metadata.agent_scratchpad_version`.

Named sessions may carry an RFC 8693 actor chain describing who the session is
acting as and on whose behalf. `agent_session_actor_chain(id?)` returns the
canonical `{sub, act}` dict for an explicit session id, or for the current
active session when `id` is omitted. Hosts bind the originating principal when a
session enters execution, and child agent sessions push their deterministic
actor name onto the chain. Snapshots expose the same value at `actor_chain` and
`metadata.actor_chain`.

Actor-chain entries may carry authority scopes as either `scopes: [string]` or
the OAuth/RFC-style `scope: "space separated"` string. The layered runtime
config section `[identity.scope_attenuation]` controls the monotonic
attenuation check. The default policy is `mode = "non-increasing"`, which means
each child actor's scope set must be a subset of its parent actor's set; equal
sets are allowed. `mode = "strict-subset"` additionally requires each hop to
drop at least one scope, and `mode = "off"` disables the check. Policy
violations surface as an auth-category error from
`actor_chain_validate_scope_attenuation(chain, opts?)`; callers that pass
`{raise: false}` receive a typed
`{kind: "scope_attenuation_violation", mode, parent_subject, child_subject,
parent_scopes, child_scopes, extra_scopes}` dict instead. When alerting is
enabled, the runtime appends a denied `identity.scope_attenuation`
OpenTrustGraph record with `metadata.actor_chain` and
`metadata.actor_chain_alert` so supervision tools can inspect the violation
without treating nested actors as authorization grants.

`agent_session_seed_from_jsonl(path, opts?)` creates a new session from a
replayable LLM transcript sidecar. Exact replay uses prompt-visible `message`
events or full request snapshots; provider-response-only sidecars are
assistant-response best effort and require `validate: false`. Options include
`truncate_to_last`, `drop_tool_calls`, `rename_session`, `validate`, `provider`,
`model`, `source_agent`, `source_session_id`, `source_kind`, `source_label`,
`source_provenance`, and `recommend_compaction`. Source metadata is recorded
under `metadata.seeded_from_jsonl.source` so hosts can explain externally
imported sessions before resuming or compacting them.

Delegated workers accept `carry.transcript_mode` to define continuation
semantics across `send_input(...)`, retriggered workers, and resumed snapshots.
`inherit` carries the completed worker transcript into the next cycle. `fork`
copies the completed transcript under a fresh transcript id and records the
source id in `metadata.parent_transcript_id`. `reset` starts the next cycle
without a carried transcript. `compact` compacts the completed worker
transcript before persistence and subsequent inheritance. Parent-facing
`worker_result` artifacts are compact summary artifacts: their `data.payload`
must omit nested full `transcript` and `artifacts` payloads by default, while
preserving request, provenance, execution profile, compact result fields, and
the ids of artifacts produced by the worker.

`agent_loop`, `sub_agent_run`, and `spawn_agent` accept a `permissions` option
that scopes one agent below the ambient capability policy. `permissions.allow`
and `permissions.deny` are tool-name glob lists or dicts keyed by tool-name
glob; dict values may be argument pattern lists or pure Harn predicates of the
tool args. Dict values may also be `path_scope` matchers, which check
configured path argument keys against the active session `workspace_anchor` and
can include mounted roots by mount mode. Deny rules win. If a call is denied
and `on_escalation` is present, the runtime calls it with a `PermissionRequest`
dict. The callback may return `false`, `true`, `{grant: "once"}`, or
`{grant: "session"}`. Session grants are memoized for the same tool and
argument payload. Child permissions are still intersected with parent `policy`
ceilings and cannot widen them. Permission decisions append `PermissionGrant`,
`PermissionDeny`, and
`PermissionEscalation` transcript events; escalation grants from `shadow` or
`suggest` trigger contexts append a `trust.promote` OpenTrustGraph record at
`act_with_approval`.

`approval_policy` is the declarative host-approval policy. It accepts legacy
`auto_approve`, `auto_deny`, `require_approval`, and `write_path_allowlist`
fields plus `rules`, a compact allow/ask/deny DSL. A rule may be written as
`{allow: {...}}`, `{ask: {...}}`, `{deny: {...}}`, or
`{action: "ask", match: {...}}`. Match dimensions include `tool`,
`tool_kind`, `side_effect`, `path`, `command`, `command_identity`, `url`,
`domain`, `method`, `mcp_server`, `mcp_tool`, `agent`, `persona`, `mode`,
`capability`, and `repeat_count_gte`. Dimensions inside a rule are ANDed;
string fields accept glob patterns. Deny beats ask, ask beats allow, and
unmatched tools are approved.

When an approval policy is active, sensitive path strings such as `.env`,
private keys, and credential files are denied by default unless
`allow_sensitive_paths: true` is set. Declared host-absolute paths outside the
workspace are denied unless `external_roots` covers the path or
`allow_external_paths: true` is set. `ask` decisions call the host via
`session/request_permission` and fail closed when no host bridge is attached.
Each approval decision produces a `harn.permission_policy_decision.v1` receipt
containing the matched rule, risk labels, normalized context, and rationale;
host approval payloads and permission transcript events carry that receipt for
audit and replay.

```harn
const budget = shared_cell({scope: "task_group", key: "tokens", initial: 0})

parallel 10 { i ->
  let updated = false
  while !updated {
    const snap = shared_snapshot(budget)
    updated = shared_cas(budget, snap, snap.value + 1)
  }
}
```

Process-local scopes are explicit:

| Scope | Meaning |
|---|---|
| `task` | Current logical task |
| `task_group` | Current parallel sibling group, or root task outside a group |
| `workflow_run` | Current workflow run when available |
| `agent_session` | Current agent session when available |
| `tenant` | Current tenant id, or `tenant_id` from options |
| `process` | Current VM process |

Durable and external state are not implicit. Use `store_*` or
`agent_state_*` for file/EventLog-backed state and host/connector APIs for
external stores.

Cells:

- `shared_cell(key_or_options, initial?)` opens a scoped cell. Options support
  `scope`, `key`, `initial`, and `tenant_id`.
- `shared_get(cell)` reads the value.
- `shared_snapshot(cell)` returns `{value, version}` for versioned CAS.
- `shared_set(cell, value)` writes with last-write-wins behavior and returns
  the previous value.
- `shared_cas(cell, expected_or_snapshot, value)` writes only when the current
  value matches the expected value, and when a snapshot is supplied, the
  version still matches. It returns `true` on success and `false` on conflict.

Maps:

- `shared_map(key_or_options, initial?)` opens a scoped map.
- `shared_map_get(map, key, default?)`, `shared_map_set(map, key, value)`,
  `shared_map_delete(map, key)`, and `shared_map_entries(map)` are the
  last-write-wins map operations.
- `shared_map_snapshot(map, key)` and
  `shared_map_cas(map, key, expected_or_snapshot, value)` provide
  versioned conflict checks.

`shared_metrics(handle)` reports `read_count`, `write_count`,
`cas_success_count`, `cas_failure_count`, `stale_read_count`, and `version`
for cells and maps.

Use named synchronization around multi-step updates:

```harn
const memo = shared_map({scope: "workflow_run", key: "memo"})
const lock = sync_mutex_acquire("memo:customer-42", 250ms)
guard lock != nil else { throw "state lock timeout" }
try {
  shared_map_set(memo, "customer-42", "summary")
} finally {
  sync_release(lock)
}
```

### Actor mailboxes

Mailboxes are scoped, named inboxes for actor-style communication between
tasks and long-lived workers. They provide targeted messages without using
transcript mutation as the transport.

```harn
const inbox = mailbox_open({scope: "task_group", name: "reviewer", capacity: 32})
spawn {
  mailbox_send("reviewer", {kind: "work", path: "src/main.rs"})
}
const msg = mailbox_receive(inbox)
```

- `mailbox_open(name_or_options, capacity?)` opens or creates an inbox.
- `mailbox_lookup(name_or_handle)` returns a handle or `nil`.
- `mailbox_send(target, value)` returns `false` when the mailbox is absent or
  closed.
- `mailbox_receive(target)` blocks until a message arrives, the mailbox closes,
  or the task is cancelled.
- `mailbox_try_receive(target)` is non-blocking.
- `mailbox_close(target)` closes the inbox to new messages.
- `mailbox_metrics(target)` reports `depth`, `capacity`, `sent_count`,
  `received_count`, `failed_send_count`, and `closed`.

### Supervisor trees

Supervisors manage named long-lived child loops and restart them according to a
small workflow-oriented policy surface. Children are closures, so the same
primitive can wrap connector streams, continuous personas, actor/mailbox loops,
workflow runners, and generic Harn task groups without new syntax.

```harn
fn poll_connector(name) {
  name
}

const sup = supervisor_start({
  name: "ops",
  strategy: "one_for_one",
  children: [
    {
      name: "github-stream",
      kind: "connector_stream",
      restart: {
        mode: "on_failure",
        max_restarts: 5,
        window_ms: 60000,
        backoff_ms: 250,
        max_backoff_ms: 30000,
        factor: 2,
        jitter_ms: 100,
        circuit_open_ms: 300000,
      },
      task: { ctx -> poll_connector(ctx.child_name) },
    },
  ],
})

const _state = supervisor_state(sup)

const _events = supervisor_events(sup)
supervisor_stop(sup, 2s)
```

`strategy` supports `one_for_one`, `one_for_all`, `rest_for_one`, and
`escalate_to_parent`. Restart modes are `never`, `on_failure`, and `always`.
Restart policies support deterministic restart caps, rolling windows,
exponential backoff, deterministic jitter, and circuit-open delay before a
suppressed child is eligible to restart again. Child status includes `running`,
`waiting`, `circuit_open`, `stopped`, `failed`, and `suppressed`.

- `supervisor_start(spec)` starts a supervisor and returns a supervisor handle.
- `supervisor_state(handle_or_id)` returns children, status, restart count,
  last error, current wait reason, active lease, next restart time, and metrics.
- `supervisor_events(handle_or_id)` returns lifecycle events for child started,
  stopped, failed, restarted, suppressed, escalated, and supervisor shutdown.
- `supervisor_metrics(handle_or_id)` returns lifecycle counters.
- `supervisor_wait(handle_or_id)` awaits the terminal lifecycle transition and
  returns the final supervisor state.
- `supervisor_stop(handle_or_id, timeout?)` requests cooperative child
  cancellation, waits for drain, then force-aborts any remaining children.

`runtime_context().debug.supervisors` exposes the same state for runtime
introspection tooling. Supervisor lifecycle events are also appended to the
active EventLog topic `supervisor.lifecycle` when an EventLog is installed.

### Channels

Channels provide typed message-passing between concurrent tasks.

```harn
const ch = channel("name", 10)   // buffered channel with capacity 10
send(ch, "hello")               // send a value, returns true
const msg = receive(ch)           // blocking receive
```

#### Channel iteration

A `for`-`in` loop over a channel asynchronously receives values until the
channel is closed and drained:

```harn
const ch = channel("stream", 10)
spawn {
  send(ch, "a")
  send(ch, "b")
  close_channel(ch)
}
for item in ch {
  log(item)    // prints "a", then "b"
}
// loop exits after channel is closed and all items are consumed
```

When the channel is closed, remaining buffered items are still delivered.
The loop exits once all items have been consumed.

#### close_channel(ch)

Closes a channel. After closing, `send` raises `ChannelClosed` and no new values
are accepted. Buffered items can still be received. Direct `receive` raises
`ChannelClosed` once the channel is closed and drained.

#### try_receive(ch)

Non-blocking receive. Returns the next value from the channel, or `nil` if
the channel is empty (regardless of whether it is closed).

### select

Multiplexes across multiple channels, executing the body of whichever
channel receives a value first:

```harn
select {
  msg from ch1 {
    log("ch1: ${msg}")
  }
  msg from ch2 {
    log("ch2: ${msg}")
  }
}
```

Each case binds the received value to a variable (`msg`) and executes the
corresponding body. Only one case fires per select.

#### timeout case

```harn
fn handle(msg) { log(msg) }
const ch1 = channel("events")
select {
  msg from ch1 { handle(msg) }
  timeout 5s {
    log("timed out")
  }
}
```

If no channel produces a value within the duration, the timeout body runs.

#### default case (non-blocking)

```harn
fn handle(msg) { log(msg) }
const ch1 = channel("events")
select {
  msg from ch1 { handle(msg) }
  default {
    log("nothing ready")
  }
}
```

If no channel has a value immediately available, the default body runs
without blocking. `timeout` and `default` are mutually exclusive.

#### select() builtin

The statement form desugars to the `select(ch1, ch2, ...)` async builtin,
which returns `{index, value, channel}`. The builtin can be called directly
for dynamic channel lists.

### Streams

Streams provide script-level lazy production of values. A stream
producer is declared with the contextual `gen fn` modifier and returns
`Stream<T>`:

```harn
gen fn numbers(start: int, end: int) -> Stream<int> {
  let n = start
  while n < end {
    emit n
    n = n + 1
  }
}
```

`gen` is contextual in this position; existing identifiers named `gen`
remain valid. `emit expr` is valid only inside a `gen fn`. It emits one
value to the consumer and resumes when the consumer pulls the next
item. The existing `yield` form keeps its current semantics and is not
used for streams.

Streams are single-pass. They can be consumed directly in `for` loops:

```harn
gen fn numbers(start: int, end: int) -> Stream<int> {
  let n = start
  while n < end {
    emit n
    n = n + 1
  }
}

for n in numbers(1, 4) {
  log(n)
}
```

They also support `.next()`, which returns `{value, done}`. When the
stream is exhausted, `done` is `true` and `value` is `nil`.

`Stream<T>` is distinct from `Generator<T>` in the type checker.
Regular functions that contain `yield` keep producing `Generator<T>`;
`gen fn` produces `Stream<T>`. Throws inside a stream body propagate to
the consumer at the pull site (`for`, `.next()`, or `.iter()`).

### Durable agent channels

Durable agent channels (epic #1870) are distinct from the in-process
`channel(...)` primitive above. Where in-process channels are typed
mailboxes between concurrent tasks inside one VM, durable channels are
a typed pub/sub primitive that writes to the active EventLog and fans
each emit out to every matching `channel.emit` trigger binding. They
survive process restarts, feed the replay oracle, and show up in the
action graph alongside webhook and cron events.

#### Emit

The runtime exposes two builtins. `emit_channel(name, payload,
options?)` appends one event to the channel's EventLog topic and
returns a `ChannelEmitReceipt`. `channel_events(name, options?)` reads
the topic oldest-first for tests and diagnostics. Reference shapes are
in `docs/src/builtins.md`; full prose is in `docs/src/agent-channels.md`.

A channel `name` resolves into the canonical form
`<scope>:<scope_id>:<name>`. Bare names default to **tenant** scope
(`tenant:<current-tenant-or-default>:<name>`). Prefixes select an
explicit scope: `session:<name>`, `pipeline:<name>`, `tenant:<name>`,
`tenant:<tenant_id>:<name>`. The `org:<org_id>:<name>` prefix is
reserved but currently rejected with `HARN-CHN-002` until org grants
ship.

#### `ChannelScope`

The scope enum is:

```text
ChannelScope ::= "session" | "pipeline" | "tenant" | "org"
```

- `session` events live in a per-process in-memory log; they are
  cleared by `reset_channel_state()` and do not cross process boundaries.
- `pipeline` and `tenant` events use the active durable EventLog; the
  resolver returns `HARN-CHN-001` if `pipeline:` is used without an
  active pipeline context.
- `org` is reserved; v1 always returns `HARN-CHN-002`.

Cross-scope isolation is automatic: distinct `tenant_id`, `session_id`,
or `pipeline_id` values resolve to distinct topics, so readers against
the wrong scope id see an empty view. Explicit `options.session_id` or
`options.pipeline_id` that conflict with the active runtime context
fail with `HARN-CHN-004` rather than silently overriding.

#### Idempotency and signed timestamps

`options.id` makes the emit idempotent. Re-emitting the same
(`name_resolved`, `id`) pair returns the original `event_id` with
`duplicate: true`. The receipt's `emitted_at` field is a signed
timestamp: HMAC-SHA-256 over `(at_ms, event_id, name_resolved, scope,
scope_id, emitted_by)` with a per-process salt and `algorithm:
"hmac-sha256"`. The replay oracle uses this to detect timestamp
tampering across runs.

#### `channel.emit` trigger source

Trigger bindings subscribe to channel events with `provider: "channel"`,
`kind: "channel.emit"`, and `match.events: ["channel:<selector>"]`.
The selector accepts the same scope prefixes as `emit_channel`:

```text
ChannelSelector ::= "channel:" Name
                  | "channel:session:" Name
                  | "channel:pipeline:" Name
                  | "channel:tenant:" TenantId ":" Name
```

The dispatched `TriggerEvent` carries the emit's `payload` verbatim at
`event.provider_payload.payload`; `event.batch` is `nil` for ordinary
single-event dispatches and populated for batched bindings (next
section).

#### `batch { count, window, key?, expire_action? }` (`BatchFilter`)

A trigger binding may declare aggregation:

```text
BatchFilter ::= {
  count: int (> 0),
  window: DurationString,
  key?: string,                  // dotted JSON path into payload
  expire_action?: "fire_partial" | "discard"   // default "fire_partial"
}
```

Semantics:

- Each filter-passing event increments a per-(`binding`, `partition_key`)
  counter. Reaching `count` invokes the handler with `event.batch`
  populated to the buffered list and resets the counter.
- `key` partitions counters by the stringified value at that JSON path
  into the channel payload. Missing paths fall back to one shared
  bucket (matches `SpawnToPool`'s "missing path = default" pattern).
- When the window elapses without reaching `count`, `expire_action`
  decides: `fire_partial` invokes the handler with whatever was
  buffered; `discard` drops the buffer silently.
- Buffers are per-process thread-local and capped at 1024 events per
  partition. Overflow drops the oldest entries with a structured
  `triggers.aggregation.buffer_overflow` warning.
- Window expiration is driven by an implicit sweep at every emit and
  by the explicit `flush_trigger_aggregations()` builtin; the latter
  is the test affordance and pairs with `mock_time(...)` /
  `advance_time(...)`.

Malformed configs (missing fields, non-positive `count`, unknown
`expire_action`, wrong-typed `key`) raise `HARN-CHN-005`.

#### `ReminderInjectHandler`

`ReminderInject(options)` (from `std/triggers`) constructs a handler-
variant dict that, on match, injects a `SystemReminder` event (see
`docs/src/system-reminders.md`) into the target session's transcript
at the next turn boundary. It is the orthogonal counterpart to
`SpawnToPool` — no spawn, no resume, no signal; the target session
keeps running and receives the typed context at the next post-turn
lifecycle pass.

```text
ReminderInjectHandler ::= {
  kind: "reminder_inject",
  target: "current" | "parent" | SessionId | (event -> SessionId?),
  body: PromptTemplate,                    // .harn.prompt
  tags?: list<string>,
  ttl_turns?: int,
  dedupe_key?: string,
  propagate?: "none" | "session",
  role_hint?: "user" | "developer",
  preserve_on_compact?: bool
}
```

`body` is rendered against `{{ event }}` (the full `TriggerEvent`),
`{{ match }}` (`matched_at`), and `{{ batch }}` (the constituent list
when batching is in effect). Missing target sessions are dropped
gracefully with a `triggers.reminder_inject.audit` audit entry rather
than failing dispatch.

#### Observability

Channel activity is published on three EventLog topics:

| Topic | Schema | Purpose |
|---|---|---|
| `lifecycle.channel.audit` | `harn.channel_emit_receipt.v1`, `harn.channel_match_receipt.v1`, `harn.channel_guardrail_audit.v1` | Durable replay/audit. One receipt per emit, per match, and per guardrail verdict. |
| `transcript.channel.lifecycle` | `transcript.channel.emit`, `transcript.channel.match` | Per-emit and per-match transcript events for live rendering. |
| `channels.<scope>.<scope_id>.<name>` | `StoredChannelEvent` | The events themselves, addressable via `event_log.subscribe(...)`. |

Every emit opens an OTel `channel.emit <name_resolved>` span; every
match opens `channel.match <name_resolved>` and links back to the
emit span via trace/span ids propagated on the trigger event headers
(this propagation passes through `batch` aggregation, so a batched
match links to all constituent emit spans).

#### Replay determinism

The replay oracle consumes `lifecycle.channel.audit` to detect:

- `HARN-REP-CHN-001` — replay matched an `event_id` with no recorded receipt.
- `HARN-REP-CHN-002` — payload hash drift on a replayed emit (producer-
  side nondeterminism).
- `HARN-REP-CHN-003` — batched match constituent ids differ across runs.

See `docs/src/observability/replay-benchmarks.md` for the oracle
interface. The user-facing reference is `docs/src/agent-channels.md`;
runnable patterns live in `docs/src/cookbooks/channels.md`.

### Agent pools

Agent pools (epic #1883) are named, concurrency-bounded worker pools
for agent work. They sit between `parallel each ... with {
max_concurrent: N }` (a local, call-site bound) and host worker tiers
(a deployment-time bound), filling the "many independent submitters
share one budget" gap. Pools are exposed through `std/lifecycle/pool`
and the host-call group registered at
`crates/harn-vm/src/stdlib/pool/mod.rs`. On a multi-thread Tokio runtime,
pool workers run as `tokio::spawn`ed child-VM isolates that inherit the VM's
pool registry handle and move `Send` VM values across the worker boundary.

#### `Pool`

```text
PoolHandle ::= {
  _type: "pool",
  id: string,                 // deterministic = sha256(scope || scope_id || name)
  name: string,
  max_concurrent: int (>= 1),
  scope: PoolScope,
  created_at: ISO8601String,
  submit: (closure, options?) -> PoolTaskHandle,
  size: () -> int,
  snapshot: () -> PoolSnapshot,
}

PoolScope ::= "session" | "pipeline" | "tenant" | "org"
```

`pool_create(options?)` allocates a handle; `pool_get(name_or_id)`
returns an existing one or `nil`; `pool_list()` enumerates the
runtime registry. Names are unique within a live VM registry, so
callers use `pool_get` to reuse an existing pool. Pipeline-scope pools
use a deterministic `(scope, scope_id, name)` id so re-creating the pool
after process restart binds to the persisted state.

#### `PoolTaskHandle`

```text
PoolTaskHandle ::= {
  _type: "pool_task",
  id: string,
  pool: string,                       // pool name
  pool_id: string,
  status: "queued" | "running" | "completed" | "failed" | "rejected",
  submitted_at: ISO8601String,
  priority: int,
  key: string | nil,
  // terminal-only:
  result: any,
  error: string | nil,
  rejection_reason: string | nil,
  rejection_policy: string | nil      // "fail_fast" | "fail_submitter" | "drop_oldest" | "drop_newest"
}
```

`pool_wait(handle_or_handles)` blocks until terminal and returns the
final snapshot (or list of snapshots). `wait_agent(handle)` from
`std/agent/workers` accepts pool task handles transparently by
matching on `_type == "pool_task"`.

#### Queue strategies

`pool_create({queue: <descriptor>})` selects how the pool dequeues
work when a worker slot frees:

```text
QueueStrategy ::= {_type: "queue_strategy", kind: "fifo" | "priority" | "lifo" | "fair_round_robin", key?: string}
```

Semantics:

- `fifo` — oldest queued first, ignoring submit `priority`.
- `priority` (default) — highest submit `priority` first, FIFO
  tiebreak.
- `lifo` — newest queued first.
- `fair_round_robin{key}` — partition queued tasks by the
  stringified value at `options.<key>` on each submit. Missing field
  shares one default partition. The runtime walks distinct
  partitions in stable order, dequeuing one task per visit; FIFO
  inside a partition.

Factories (`std/lifecycle/pool`): `fifo()`, `priority()`, `lifo()`,
`fair_round_robin(key = "key")`. `QueueStrategy()` returns the four
as a dict for dotted access.

#### Backpressure

`pool_create({backpressure: <descriptor>})` bounds the queue and
selects the overflow policy:

```text
Backpressure ::= {_type: "backpressure", kind: "queue" | "fail_fast" | "ring_buffer", max_depth?: int, on_full?: OnFullPolicy, capacity?: int}

OnFullPolicy ::= "block_submitter" | "drop_oldest" | "drop_newest" | "fail_submitter"
```

Semantics:

- `queue{max_depth, on_full}` — bound queued tasks at `max_depth`.
  `block_submitter` (default) parks the submitting fiber until a
  slot frees. `drop_oldest` evicts the oldest queued task as a
  `rejected` handle and accepts the new submit. `drop_newest`
  rejects the new submit. `fail_submitter` raises `HARN-POL-001` at
  the submit call site.
- `fail_fast{}` — raise `HARN-POL-002` synchronously when no worker
  slot is immediately free; no queue is retained.
- `ring_buffer{capacity}` — retain the newest `capacity` queued
  tasks by evicting the oldest queued task on overflow.

Drop policies (`drop_oldest`, `drop_newest`, `ring_buffer`) emit a
`pool_drop` audit on `lifecycle.pool.audit` with the pool name,
evicted task id, policy, queue depth, and max depth. The submitter
sees a task handle whose terminal snapshot is
`{status: "rejected", rejection_reason, rejection_policy}`.

#### Scopes and durability

| Scope | Storage | Survives | Notes |
|---|---|---|---|
| `session` | VM-scoped in-memory registry. | VM/session lifetime. | Default. Zero I/O. |
| `pipeline` | JSONL append-log under `.harn/pools/<pipeline_id>__<pool_name>.jsonl`. | Process restart, within one pipeline. | Terminal and stale task metadata reload on next `pool_create({scope: "pipeline", ...})`. |
| `tenant`, `org` | Host-managed registry. | Tenant / org lifetime. | Currently rejected by the in-process runtime unless an embedding host implements tenant/org pool routing. |

On pipeline-scope reload, persisted `Queued` or `Running` task records
are restored as failed stale markers because the in-memory closure body
cannot be reconstructed after process death. Callers that need retry
semantics resubmit with an `idempotency_key`; the stale marker preserves
the audit trail and allows the fresh submit to execute as a new task.
`stale_after_ms` (default 30000 ms; configurable via
`opts.stale_after_ms`) is retained as the freshness knob for future host
backends. The store compacts opportunistically when
`max_concurrent + |queue| + |terminal-since-compaction|` exceeds an
internal threshold. `pool_simulate_restart()` drops the in-process
registry without touching disk and is the conformance affordance for
reload tests.

#### Idempotency

`pool.submit(closure, {idempotency_key: K, ...})` short-circuits when
`(pool_id, K)` already exists in the idempotency index. The second
call returns the *same* task handle (`id` matches the first) and, if
the first task is terminal, the terminal snapshot. Pipeline-scope
pools persist the index so resubmit across restart preserves the
contract.

#### `spawn_to_pool` trigger handler

A trigger binding may route matched events into a named pool by
declaring a `SpawnToPool` handler (from `std/triggers`):

```text
SpawnToPoolHandler ::= {
  kind: "spawn_to_pool",
  pool: string,                             // pool name or id
  task_factory: (event -> closure),         // builds the per-event closure
  priority_from?: string,                   // dotted JSON path; missing -> 0
  key_from?: string                         // dotted JSON path; missing -> default partition
}
```

The dispatcher resolves `pool` via `pool_get`, invokes
`task_factory(event)`, and submits the resulting closure under the
pool's queue strategy + backpressure policy. The resulting pool task
id is recorded on the trigger match receipt so the replay oracle can
verify the same event maps to the same task across runs. A missing
pool name lands in `trigger.dlq`.

#### Observability

Pool activity is published on one EventLog topic:

| Topic | Records |
|---|---|
| `lifecycle.pool.audit` | `pool_submit`, `pool_dequeue`, `pool_drop` |

Every submit opens an OTel `pool.submit <pool_name>` span; every
dequeue opens `pool.dequeue <pool_name>` and links back to the
submit span so async-boundary traces remain stitched.

Pool tasks surface in pipeline lifecycle drains:
`harness.unsettled_state().pool_pending_tasks` lists tasks blocking
pipeline finalization (see `docs/src/stdlib/lifecycle.md`).

The user-facing reference is `docs/src/agent-pools.md`; the stdlib
reference is `docs/src/stdlib/lifecycle-pool.md`; runnable patterns
live in `docs/src/cookbooks/pools.md`.
