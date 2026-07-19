<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

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

`pipeline_on_finish(callback)` is a stdlib builtin that registers a
`fn(harness, return_value)` closure into a thread-local one-shot slot
(`PIPELINE_ON_FINISH`). The slot is last-write-wins inside one run.
`Vm::execute` consumes the registered callback via
`take_pipeline_on_finish` exactly once, between the `PreFinish` and
`OnUnsettledDetected` gates. The callback's return value replaces the
pipeline's return value before `PostFinish` fires. On the error exit
path the slot is cleared so a failed pipeline cannot leak its
registration into the next run.

### `Harness` type

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

Capability sub-handles are exposed by field access on the harness:

| Field | Type | Primary methods |
|---|---|---|
| `harness.stdio` | `HarnessStdio` | `print`, `println`, `eprint`, `eprintln`, `read_line`, `prompt` |
| `harness.term` | `HarnessTerm` | `width`, `height`, `read_password` |
| `harness.clock` | `HarnessClock` | `now_ms`, `timestamp`, `monotonic_ms`, `elapsed`, `sleep_ms` |
| `harness.fs` | `HarnessFs` | `read_text`, `write_text`, `replace_text`, `append`, `exists`, `list_dir` |
| `harness.env` | `HarnessEnv` | `get`, `set`, `unset`, `list` |
| `harness.random` | `HarnessRandom` | `gen_u64`, `uuid`, `bytes` |
| `harness.net` | `HarnessNet` | `get`, `post` |
| `harness.system` | `HarnessSystem` | `platform`, `arch`, `cpu`, `memory`, `cwd`, `pid` |

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

The runtime exposes 40 hook events. Registration surfaces:
`register_tool_hook` (tool events), `register_persona_hook` (persona
events), `register_worker_hook` (worker events), and
`register_session_hook` (session events).

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
   time. Wall-clock fields (`queued_at_ms`, `age_ms`) come from
   `clock_mock`-aware sources and respect `mock_time(...)` /
   `advance_time(...)` in tests.
4. **One-shot registration.** `pipeline_on_finish(callback)` is
   last-write-wins; the slot is consumed exactly once per run via
   `take_pipeline_on_finish`. The error exit path clears the slot
   alongside the audit log, partial-handoff registry, disposition
   slot, and seq counter, so a failed run cannot leak in-progress
   state into the next run.

The user-facing reference is `docs/src/pipeline-lifecycle.md`; the
stdlib reference is `docs/src/stdlib/lifecycle.md`; runnable patterns
live in `docs/src/cookbooks/lifecycle.md`.
