## Pipeline lifecycle

Pipelines do not end the moment their declared steps return. Between
the last statement of the pipeline body and the value the host sees,
the runtime fires a fixed sequence of lifecycle gates and one user
callback. The same callback shape — `fn(harness, return_value) ->
return_value` — threads through every gate, so presets and
combinators that work for `on_finish` also work for hook handlers,
`resume_by` callbacks, and any custom drain logic.

### Lifecycle event order

When a pipeline's declared steps complete the runtime walks this
sequence on the main VM:

1. `PreFinish` — last chance to inject a reminder before the pipeline
   value is captured. Rejects `{block: true}`; the runtime surfaces a
   runtime error pointing at `OnFinish.block_until_settled`.
2. The registered `on_finish` callback. Default behavior (no
   registration) is identical to `on_finish_abandon`.
3. `OnUnsettledDetected` — fires after the callback if any bucket in
   `harness.unsettled_state()` is non-empty. Accepts `{block: true,
   reason}` to delay finish until the host explicitly drains and
   `{modify: payload}` to amend the snapshot.
4. `PostFinish` — advisory; observe the final value, push telemetry.
5. The value is returned to the host.

Each step records `hook_call` / `hook_returned` / `hook_vetoed`
events on the active session transcript so replay reproduces the same
control flow.

### `Pipeline.on_finish` semantic

`harness.agent.pipeline_on_finish(callback)` registers a
`fn(harness, return_value)` closure into a thread-local one-shot slot
(`PIPELINE_ON_FINISH`). The slot is last-write-wins inside one run.
`Vm::execute` consumes the registered callback via
`take_pipeline_on_finish` exactly once, between the `PreFinish` and
`OnUnsettledDetected` gates. The callback's return value replaces the
pipeline's return value before `PostFinish` fires. On the error exit
path the slot is cleared so a failed pipeline cannot leak its
registration into the next run.

### `Harness` type

Effects flow from the harness capability object; imports are pure.

`Harness` is the only root authority injected by the runtime. A program
receives it as an explicit entrypoint parameter—normally
`fn main(harness: Harness)`—and may pass either the root or one of its nominal
sub-handles to helpers. Importing a module never grants authority: imports only
bind code, types, constants, and pure values. An imported helper that performs
an effect must require the relevant `Harness*` value in its ordinary parameter
list.

Pure computation remains available as global functions. Runtime operations are
not globals: their public contracts are methods on a closed `Harness`
capability type. Each contract declares its conservative effect family,
access mode, and resource selector; the compiler, policy engine, runtime
receipts, language tooling, and generated reference all consume that same
manifest. A host may implement a contract directly or through a privileged
wire primitive, but privileged wire names are not source-visible or
re-exportable.

Possessing one sub-handle grants no other sub-handle. For example, a helper
that accepts `HarnessFs` cannot read environment variables, invoke a process,
or make a network request. Hosts may narrow a grant or supply deterministic
fixtures without installing a process registry or an ambient thread-local
mock. Runtime dispatch rejects a method absent from the typed contract even if
an embedder accidentally registered a same-named callable.

Authority narrows as control moves inward. Entrypoints and orchestration
functions may accept root `Harness`; ordinary helpers should accept the
smallest coherent nominal capability interface that describes their work.
One handle is not automatically better than two, and a coordinator that
genuinely combines several capabilities may retain the root. A helper that
needs one capability accepts that `Harness*` handle. A helper that needs two
accepts them as one record, such as `{fs: HarnessFs, tools: HarnessTools}`,
which the caller builds as `{fs: harness.fs, tools: harness.tools}`. A record
keeps the grant as narrow as two separate parameters would, and names each
capability at the call site so two handles of similar shape cannot be swapped
by mistake. The `capability-attenuation` lint reports both shapes, and offers
to rewrite the signature and its call sites together when the surrounding code
proves the rewrite is safe.

`Harness` and every `Harness*` sub-handle are runtime authority, not domain
data. They cannot be JSON-serialized, placed in the persistent store,
checkpointed, or embedded inside a persisted record. Programs persist stable
identifiers and ordinary data, then receive fresh authority from the runtime
when execution resumes.

The single argument to every lifecycle callback is the harness. The
read-side surface is `unsettled_state(): UnsettledStateSnapshot`,
which returns a stable JSON-shaped dict with five lists:
`suspended_subagents`, `queued_triggers`, `partial_handoffs`,
`in_flight_llm_calls`, and `pool_pending_tasks`. The `is_empty`,
`counts`, and `summary` derived methods accept either an already-taken
snapshot (for callback-consistent decisions) or no argument (fresh
snapshot per call). Producers populate buckets from live VM
registries (suspended subagents, partial handoffs, in-flight LLM
calls, pool pending tasks) and from event-log records (queued
trigger inbox + worker queue items).

Capability sub-handles are exposed by field access on the harness. The
contract manifest is the exhaustive method reference; these groups describe
the stable ownership boundaries:

| Fields | Ownership |
|---|---|
| `stdio`, `term`, `clock`, `env`, `random`, `system` | Process observation, user I/O, time, and entropy |
| `fs`, `process`, `net`, `channels`, `secrets` | External I/O and durable communication |
| `llm`, `agent`, `tools`, `interaction`, `verdict` | Model, worker, tool, human-interaction, and decision authority |
| `tenant`, `auth`, `obs`, `runtime`, `project` | Identity, authentication, telemetry, runtime, and project state |
| `dashboard`, `workspace`, `session`, `permission` | Host presentation, workspace services, session facts, approvals |
| `text`, `lsp`, `credentials` | Host text analysis, language services, and product credential custody |
| `merge_captain`, `pr_monitor`, `workflow` | Repository integration and durable host-workflow services |
| `ast`, `code_index`, `scanner`, `rules`, `lint` | Language and code-analysis services |
| `computer`, `embed`, `memory`, `sqlite`, `postgres` | Native interaction and data services |
| `fs_watch`, `host_lease`, `secret_store`, `terminal` | Long-lived resource factories |
| `testing` | Per-harness deterministic fixtures, captured calls, and virtual-clock control |

Optional host protocol methods are part of this same typed surface. A host may
implement `harness.workspace.search(request)` or
`harness.project.peer_message(request)` through its bridge, but a script cannot
add a method of its own or reach the host by passing a method name as a string.
One capability registry drives type checking, effect inference, runtime
dispatch, fixture validation, and contract exports. A host's capability
manifest says which optional methods that host implements: it can remove a
method from what a script may call, never add one.

Write-side actions:

| Method | Effect |
|---|---|
| `resume_subagent(handle, input?)` | Resume a suspended worker; falls back to send-input for awaiting retriggerables. |
| `cancel_subagent(handle, reason?)` | Close a suspended worker via `__host_worker_close`. |
| `handoff_to(target_pipeline, payload?)` | Record a `PartialHandoffEnvelope` in the thread-local registry; returns `{status: "queued", envelope}`. |
| `acknowledge_trigger(id)` | Settle a queued inbox or worker-queue item with the existing ack record. |
| `defer_trigger(id, target_pipeline?)` | Ack the trigger and record a partial-handoff envelope (default target `deferred-triggers`). |
| `acknowledge_handoff(envelope_id, decision?)` | Remove a partial envelope from the registry; emit `handoff_acknowledged` audit. |
| `wait_for_any_settlement(max_duration?)` | Snapshot + return `{status, timed_out, state}`. |
| `emit_audit(kind, payload?)` | Append a `LifecycleAuditEntry` to the per-run log and (when an EventLog is installed) the `pipeline.lifecycle.audit` topic. |
| `finalize(disposition?)` | Stamp the run's final disposition; emit `pipeline_finalized`. |
| `spawn_settlement_agent(unsettled, return_value)` | Hand off to the bounded settlement-agent drain loop. |
| `current_pipeline_id()` | Run id from the current mutation session, else session id, else nil. |

### `DrainAgent` constrained tool surface + ordering enforcement

The settlement-agent loop (`harness.spawn_settlement_agent`) walks the
unsettled snapshot in a fixed canonical order: suspended subagents →
queued triggers → partial handoffs → in-flight LLM calls → pool
pending. Each item receives a default disposition:

| Bucket | Default disposition |
|---|---|
| `suspended_subagents` | Cancel via `harness.cancel_subagent`. |
| `queued_triggers` | Acknowledge via `harness.acknowledge_trigger`. |
| `partial_handoffs` | Acknowledge as `deferred` via `harness.acknowledge_handoff`. |
| `in_flight_llm_calls` | Drain via the LLM call registry. |
| `pool_pending_tasks` | Defer via the pool registry. |

The loop is bounded by a per-call budget — default 5, configurable via
the third arg to `spawn_settlement_agent`, hard-capped at 20. On
exhaustion a `drain_unsettled_remaining` audit captures the
remainder. Each disposition records a `drain_decision` lifecycle
audit and fires the `OnDrainDecision` hook chain (`Allow` / `Block` /
`Modify`) before persisting, so VM-side hooks observe the disposition
before it lands.

Ordering enforcement: `harness.acknowledge_trigger` and
`harness.acknowledge_handoff` reject out-of-order calls with
`HARN-DRN-001`. A caller (the settlement-agent loop or a future
LLM-driven settlement variant) cannot finalize a later category while
earlier work is still pending. `__host_settlement_agent_active()`
returns `true` when the constrained drain tool surface is in scope so
conformance fixtures and IDE hosts can observe the loop boundary.

### Lifecycle event taxonomy

The runtime exposes 40 hook events. Registration surfaces live on typed
capabilities: `harness.tools.register_hook` (tool events),
`harness.agent.register_persona_hook` (persona events),
`harness.agent.register_worker_hook` (worker events), and
`harness.agent.register_session_hook` (session events).

Every VM-backed hook handler has the entrypoint shape
`fn(harness: Harness, event)`. The runtime supplies the exact root Harness for
the firing execution. This is an orchestration boundary, so root authority is
appropriate here; ordinary helpers called by a hook SHOULD accept the
narrowest coherent nominal handle they require. A package-manifest `[[hooks]]`
export uses the same ABI as a programmatically registered closure.

| Event | Category | Reminder effects |
|---|---|---|
| `PreToolUse`, `PostToolUse` | tool | supported |
| `PreAgentTurn`, `PostAgentTurn` | persona | supported |
| `WorkerSpawned`, `WorkerProgressed`, `WorkerWaitingForInput`, `WorkerSuspended`, `WorkerResumed`, `WorkerCompleted`, `WorkerFailed`, `WorkerCancelled` | worker | rejected (`HARN-RMD-002`) |
| `PreStep`, `PostStep` | persona | supported |
| `OnBudgetThreshold` | persona | supported |
| `OnApprovalRequested`, `OnHandoffEmitted` | persona | supported |
| `OnPersonaPaused`, `OnPersonaResumed` | persona | supported |
| `SessionStart`, `SessionEnd` | session | supported |
| `UserPromptSubmit` | session | supported, accepts `{block, reason}` |
| `PreCompact`, `PostCompact` | session | supported |
| `PostTurn` | session | supported |
| `PermissionAsked`, `PermissionReplied` | session | accepts `{decision: "allow"\|"deny"\|"ask", reason}` |
| `FileEdited` | session | supported (drained per-turn) |
| `SessionError`, `SessionIdle` | session | supported |
| `PreFinish` | session | supported; rejects `{block: true}` |
| `PostFinish` | session | supported (advisory) |
| `OnUnsettledDetected` | session | accepts `{block, reason}` and `{modify: payload}` |
| `PreSuspend`, `PostSuspend`, `PreResume`, `PostResume` | session | suspend / resume gates |
| `PreDrain`, `PostDrain`, `OnDrainDecision` | session | drain-loop gates |

Veto-capable events accept `{block: true, reason}`. Lifecycle-gate
events that support payload rewriting also accept `{modify: payload}`
to amend the dispatched event before resuming the lifecycle step.
Hook returns also accept a reminder effect (`{reminder: {...}}` or a
bare reminder spec) on every event whose `supports_reminder_effects()`
is true. Worker events reject reminder effects with diagnostic
`HARN-RMD-002` because the worker dispatcher does not own a session
transcript.

### Replay determinism rules

Every lifecycle decision is reproducible on a replay:

1. **Cached resume input.** `harness.resume_subagent(handle, input)`
   persists the input snapshot on the resume event so the replay
   oracle feeds the same value back into the same worker without
   re-reading host state.
2. **Memoized drain decisions.** Each `drain_decision` audit captures
   the bucket, item id, and disposition. The replay oracle consumes
   the audit log instead of re-walking the snapshot, so a
   non-deterministic `on_drain_decision` hook (one that consults
   wall-clock or external state) cannot drift the second run.
3. **Signed timestamps.** `harness.emit_audit` stamps entries with a
   per-run monotonic `LIFECYCLE_SEQ` counter rather than wall-clock
   time. Wall-clock fields (`queued_at_ms`, `age_ms`) come from the
   harness clock. Tests control only their own harness through
   `harness.testing.clock_set(...)`, `clock_advance(...)`, and
   `clock_reset()`.
4. **One-shot registration.** `harness.agent.pipeline_on_finish(callback)`
   is last-write-wins; the slot is consumed exactly once per run via
   `take_pipeline_on_finish`. The error exit path clears the slot
   alongside the audit log, partial-handoff registry, disposition
   slot, and seq counter, so a failed run cannot leak in-progress
   state into the next run.

The user-facing reference is `docs/src/pipeline-lifecycle.md`; the
stdlib reference is `docs/src/stdlib/lifecycle.md`; runnable patterns
live in `docs/src/cookbooks/lifecycle.md`.
