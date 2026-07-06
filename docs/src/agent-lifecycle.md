# Agent lifecycle: suspend, resume, stop, self-park

Harn agents are not just one-shot calls — they are cooperatively schedulable
units that can park mid-loop, persist a resumable snapshot, resume later, or
hand unfinished child work back to a parent with a typed stop handoff. This
page is the long-form reference for lifecycle primitives: when to reach for
them, who owns the resume responsibility, and how transcript continuity or
stop handoffs work across the boundary.

For a one-screen LLM quickref see the "Agent lifecycle: pause, resume,
self-park" section in `docs/llm/harn-quickref.md`. For the upstream protocol
contributions that mirror this surface see
[ACP `session/suspend`](./protocol-contributions/acp-session-suspend.md) and
[A2A `TaskState.PAUSED`](./protocol-contributions/a2a-paused-state.md).

## When to suspend

Suspend/resume is one of several ways Harn lets an agent yield. Pick the
narrowest one that fits.

| Goal | Use | Why |
|---|---|---|
| End the loop because the work is done | Natural completion (`status: "done"`) | No state to preserve. The transcript is final. |
| Cap tokens or wall-clock time | `max_iterations`, `token_budget`, `autonomy_budget` | Budgeting is policy, not a checkpoint. The loop terminates with `status: "budget_exhausted"` or `status: "approval_required"`. |
| Wait for one specific external event | **Suspend/resume** with `agent_await_resumption(reason, conditions)` | Park the agent, declare what should wake it, keep the transcript. |
| Park indefinitely until an operator resumes by hand | **Suspend/resume** with `agent_await_resumption(reason)` (no conditions) | Parent agent or operator owns the resume; no trigger gets registered. |
| Park one agent while another runs to completion | Parent-driven `subagent_pause(handle, reason)` plus `subagent_resume(handle)` | The parent loop owns when the child resumes. |
| Stop a child and take over its unfinished work | `agent_stop(handle, {graceful: true})` or parent-driven `subagent_stop(handle)` | The runtime folds the child transcript and descendant subagent summaries into a typed handoff artifact. |
| Background-run an agent and rejoin it later | `spawn_agent({background: true})` + `wait_agent(handle)` | The agent is *running*, not parked. Use suspend/resume only when the agent itself should yield CPU. |

Two anti-patterns worth calling out:

- **Suspend is not a sleep.** The runtime does not poll for resume — it
  registers conditions (when present) with the trigger dispatcher, persists
  a snapshot, and exits the loop frame. A polling sleep keeps the worker
  alive on a thread; suspend releases it. If you want fixed-cadence wakeup,
  pass `conditions.timeout: {duration_minutes: N}`.
- **Suspend is cooperative.** A suspend request flips a flag that is
  honored at the next turn boundary. Tool calls and LLM requests in flight
  finish first. A worker that does not return to a turn boundary (e.g. an
  infinite shell command in a single tool) will never observe the request.

## The lifecycle surface

The suspend/resume primitive ships as three builtin layers:

1. **Script-level** — `suspend_agent`, `resume_agent`, `agent_stop`,
   `parse_resume_conditions`, `agent_await_resumption` from `std/agent/workers`. Operators and parent
   pipelines call these directly.
2. **Model-facing tools** — `agent_loop(...)` automatically exposes
   `agent_await_resumption` as a callable tool to the model so an agent can
   self-park. Pass `subagents: true` to also expose `subagent_pause`,
   `subagent_resume`, and `subagent_stop`.
3. **Resume-responsibility callbacks** — `ResumeBy.parent_llm`,
   `ResumeBy.local_runtime`, `ResumeBy.cloud_harness`, and
   `ResumeBy.pipeline_drain` from `std/agent/resume_by` name *who* owns
   resuming the suspended agent. The runtime calls `default_resume_by(...)`
   when the script does not supply one.

### Builtins

| Function | Purpose |
|---|---|
| `suspend_agent(worker, reason?, options?)` | Cooperatively suspend a running worker. Persists a resumable snapshot, emits a `WorkerSuspended` lifecycle event, returns `status: "suspended"` with `suspension` metadata. Idempotent on already-suspended workers. |
| `resume_agent(worker_or_snapshot, input?, continue_transcript?)` | Resume a suspended worker, optionally with new input. `continue_transcript` defaults to `true` (full transcript preserved); pass `false` to resume from prior summary plus new input only. Accepts either a live handle or a persisted snapshot dict. |
| `agent_stop(worker, options?)` | Stop a worker. The default is a hard cancel; `{graceful: true}` emits `WorkerStopped` and returns `{status: "stopped", handoff, children, handoffs, worker}` where `handoff` is a normalized handoff artifact and `children` recursively folds descendant subagent handoffs. |
| `agent_await_resumption(reason, conditions?, resume_by?)` | Build the normalized lifecycle-tool request. Inside an `agent_loop` running as a worker, the loop intercepts this call structurally and routes it through the same suspend path as `suspend_agent`. |
| `parse_resume_conditions(conditions?)` | Validate and normalize a `ResumeConditions` dict without spawning a worker. Useful for input validation in handlers and pre-flight checks. |
| `agent_lifecycle_tools(registry?, options?)` | Decorate a tool registry with `agent_await_resumption` (always) and, when `subagents: true`, `subagent_pause` / `subagent_resume` / `subagent_stop`. `agent_loop` calls this internally; scripts compose it when they build registries by hand. |

### Resume conditions

`ResumeConditions` is the shared shape consumed by both `agent_await_resumption`
and `spawn_agent({options: {resume_when: ...}})`:

```text
{
  trigger?:  TriggerSpec,    // any std/triggers trigger spec
  timeout?:  {duration_minutes: int, on_timeout?: string},
  on_event?: string,         // EventLog topic name
}
```

All three fields are optional. When all are omitted the worker is parked
"open" — only the parent agent, an operator, or `resume_agent(...)` can
wake it. `parse_resume_conditions(nil)` returns `nil`; bad input raises
`HARN-SUS-002` with the failing field path.

`timeout.on_timeout` defaults to `"resume_with_summary"` and accepts
`"fail"` or `"resume_with_input"`. `trigger` is validated by the same
trigger-spec parser used by `trigger_register(...)`, so any provider that
works as a trigger source works as a resume condition.

## Self-park mid-loop

When `agent_loop(...)` exposes `agent_await_resumption` as a model tool,
the model can park itself between turns. The loop intercepts the call
*before* normal tool dispatch, validates conditions, persists a snapshot,
and returns a structured result to the parent (or the direct caller, for
top-level loops):

```harn,ignore
import { agent_await_resumption } from "std/agent/workers"

const result = agent_loop("Wait for the maintainer's review.", nil, {
  provider: "openai",
  model: "gpt-5",
  tool_format: "native",
  // agent_await_resumption is registered automatically.
})

// The model decided to park; `result.status == "suspended"`.
if result.status == "suspended" {
  log(result.reason)                      // model-supplied
  log(result.initiator)                   // "self"
  log(result.conditions?.timeout?.duration_minutes)
  log(result.handle.snapshot_path)        // persisted snapshot
}
```

`status: "suspended"` carries `handle`, `reason`, `initiator` (one of
`"self"`, `"parent"`, `"operator"`, `"triggered"`), `conditions`, and
`iterations_completed` (turns finished before the park). The handle is a
resumable worker reference; the snapshot is a JSON document on disk.

Restore the snapshot from the CLI in a fresh process:

```bash
harn run --resume .harn/workers/worker_01HF...json
```

`harn run --resume <path>` rehydrates the worker, replays the resume-
continuity reminder onto the next turn, and continues the loop in the same
session.

## Parent-driven pause, resume, and stop

For multi-agent setups, the parent loop owns pause/resume of its children.
Pass `subagents: true` (or `subagent_tools: true`) to `agent_loop(...)` to
expose `subagent_pause`, `subagent_resume`, and `subagent_stop` as
model-callable tools:

```harn,ignore
import { agent_lifecycle_tools } from "std/agent/workers"

const parent_registry = agent_lifecycle_tools(tool_registry(), {subagents: true})

agent_loop("Coordinate the review.", nil, {
  tools: parent_registry,
  subagents: true,
  provider: "openai",
})
```

The parent's model can now call:

| Tool | Effect |
|---|---|
| `subagent_pause(handle, reason)` | Pause a running child after its current turn settles. Idempotent on already-suspended children. |
| `subagent_resume(handle, input?, continue_transcript? = true)` | Resume a suspended child with optional new input. |
| `subagent_stop(handle, graceful? = true, reason?)` | Stop a child. Graceful mode is the default and returns a recursive typed handoff summary; `graceful: false` preserves hard-cancel behavior. |

Scripts can drive the same lifecycle directly without going through the
model:

```harn,ignore
const handle = sub_agent_run("Draft the changelog.", {
  background: true,
  provider: "openai",
})

const snapshot = suspend_agent(handle, "operator pulled context")
// ... do other work ...
const resumed = resume_agent(handle, "Pick up where you left off.")
const final = wait_agent(handle)
log(final.status)         // "done"
log(final.has_transcript) // true — transcript continuity preserved
```

`subagent_pause` and `subagent_resume` emit `tool_call_audit` telemetry
with `initiator: "parent"` so the trust graph can distinguish parent-
driven pauses from self-parks.

`subagent_stop` emits the same audit telemetry and, in graceful mode, returns
a handoff artifact shaped for parent takeover. `handoff.metadata` includes
the child `session_id`, workspace anchor, token budget/usage, snapshot path,
and `child_handoffs` for recursively stopped descendant subagents.

## Conditioned resume

Pair `agent_await_resumption` with `conditions` to declare what should
wake the parked agent. The runtime registers the trigger (if any) with
the local dispatcher and persists the conditions alongside the snapshot
so they survive restart:

```harn,ignore
import { agent_await_resumption } from "std/agent/workers"

// Self-park until either a review approval lands OR 30 minutes pass.
const request = agent_await_resumption("waiting on review", {
  trigger: {
    kind: "review.approved",
    provider: "github",
    match: {events: ["review.approved"]},
  },
  timeout: {
    duration_minutes: 30,
    on_timeout: "resume_with_summary",
  },
})
```

`on_timeout` semantics:

- `"resume_with_summary"` (default) — resume with a system reminder
  summarizing what happened during the wait.
- `"fail"` — terminate the worker with `status: "failed"`.
- `"resume_with_input"` — resume with a caller-supplied input string.

`spawn_agent({options: {resume_when: ...}})` accepts the same shape so a
worker can be born already parked:

```harn,ignore
import { parse_resume_conditions, spawn_agent } from "std/agent/workers"

const resume_when = parse_resume_conditions({
  trigger: {
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:pr.merged"]},
  },
  timeout: {duration_minutes: 60, on_timeout: "fail"},
})

spawn_agent({
  task: "Cut a release once the PR merges.",
  node: {
    kind: "subagent",
    mode: "llm",
    model_policy: {provider: "openai", model: "gpt-5"},
    output_contract: {output_kinds: ["summary"]},
  },
  options: {resume_when: resume_when},
})
```

## Resume responsibility (`ResumeBy.*`)

Every suspension has exactly one resume owner. The optional `resume_by`
argument on `agent_await_resumption` names that owner with a callback:

| Callback | When it runs |
|---|---|
| `ResumeBy.parent_llm` | Parent agent's LLM resumes the child via `subagent_resume`. The default when no `conditions` are present. |
| `ResumeBy.local_runtime` | Local trigger dispatcher registers `conditions.trigger` and calls `resume_agent` on fire. The default when `conditions` are present and no cloud session is bound. |
| `ResumeBy.cloud_harness` | A cloud webhook receiver persists the suspension across process restart and resumes when the configured event lands. Selected when a cloud session is bound. |
| `ResumeBy.pipeline_drain` | The enclosing pipeline's drain step owns the resume responsibility. Use inside lifecycle combinators. |

Each callback is a pure closure `(harness, suspension) -> {handled, mechanism}`
so they compose with `first_available(...)`, `compose(...)`, and the rest of
`std/lifecycle/combinators`:

```harn,ignore
import { ResumeBy, first_handled } from "std/agent/resume_by"

const RB = ResumeBy()
const chain = first_handled([RB.cloud_harness, RB.local_runtime, RB.parent_llm])

const request = agent_await_resumption(
  "wait for upstream merge",
  {trigger: {kind: "channel.emit", provider: "channel", match: {events: ["channel:upstream.merged"]}}},
  chain,
)
```

`default_resume_by(...)` mirrors the runtime's default policy and is
publicly exported so scripts can call it explicitly:

- `conditions == nil` → `ResumeBy.parent_llm`
- `conditions != nil`, no cloud session → `ResumeBy.local_runtime`
- `conditions != nil`, cloud session → `first_handled([cloud_harness, local_runtime])`

Every resolved dispatch emits a `resume_by_dispatched` audit through
`harness.emit_audit`. Declined entries (e.g. `cloud_harness` with no
cloud session) emit `resume_by_declined`. Both surface through
`lifecycle_audit_log_take()` for tests and observability.

## Transcript continuity

By default a resumed worker carries its full transcript forward. The
runtime injects a single-shot `system_reminder` with `dedupe_key:
"resume_continuity"` describing the gap, the suspend reason, and (when
available) what fired the resume. The reminder is consumed on the next
turn and does not re-apply on subsequent suspends.

Set `continue_transcript: false` on `resume_agent(...)` to drop the
recorded turns and restart from the prior summary plus new input:

```harn,ignore
// Discard the long deliberation transcript; restart from a clean state
// with a fresh prompt.
resume_agent(handle, "Try a completely different approach.", false)
```

This is the right call when the previous deliberation hit a dead-end and
you want the model to think from first principles, not "patch over" the
prior reasoning.

## Top-level loops and `--resume`

A root `agent_loop(...)` — one called from a pipeline or `harn run`
script, not as a child of another agent — uses the same suspend path. On
self-park the runtime persists a snapshot under `.harn/workers/worker_*.json`
and returns `status: "suspended"` to the caller. The CLI can cold-restore
that snapshot in any subsequent process:

```bash
# First run: agent parks itself, prints the snapshot path.
harn run scripts/triage.harn
# status=suspended
# snapshot=.harn/workers/worker_01HFEX...json

# Later, in a fresh process:
harn run --resume .harn/workers/worker_01HFEX...json --json
# {"event_type": "done", "value": {"status": "done", ...}}
```

`--resume` accepts both absolute and script-relative snapshot paths. The
resumed loop emits its final value as a JSON event when `--json` is set
so callers can pipeline it without parsing prose output.

For cross-runtime handoff, package the suspended snapshot as a session bundle
instead of copying the raw file path by hand:

```bash
harn session checkpoint .harn/workers/worker_01HFEX...json \
  --out checkpoint.bundle.json
harn session import checkpoint.bundle.json \
  --worker-snapshot-dir imported-workers \
  --json
harn run --resume imported-workers/worker_01HFEX...json --json
```

`session checkpoint` defaults to a local resumable bundle: it may contain
unredacted transcript/tool payloads and should be handled like the original
worker snapshot. Use `--sanitized` or `--replay-only` only when the bundle is
for share-safe inspection rather than direct continuation.

## Daemon idle is a degenerate case

`agent_loop(..., {daemon: true})` and the
[daemon stdlib wrappers](./stdlib/daemon.md) build on the same primitive.
When a daemon idles waiting for a trigger event or a wake-interval tick,
the stdlib records an `agent_await_resumption` request internally and
parks the loop. Daemon-specific snapshot fields
(`pending_event_count`, `queued_event_count`, `inflight_event`,
`wake_interval_ms`, `watch_paths`) are persisted alongside the standard
suspend metadata.

That means daemons can be cold-restored with `daemon_resume(path)` the
same way a parked agent can be cold-restored with `harn run --resume`.

See [Agent loops — Daemon stdlib wrappers](./llm/agent_loop.md#daemon-stdlib-wrappers)
for the daemon-specific surface.

## Diagnostic codes

| Code | Meaning |
|---|---|
| `HARN-SUS-001` | `suspend_agent` was called on a worker that is not running. |
| `HARN-SUS-002` | `ResumeConditions` validation failed. |
| `HARN-SUS-003` | `resume_agent` was called on a live worker that is not suspended. |
| `HARN-SUS-004` | A resume snapshot was missing, stale, unreadable, or version-incompatible. |
| `HARN-SUS-005` | `agent_await_resumption` was invoked outside `agent_loop` structural handling (e.g. called directly as a tool handler). |
| `HARN-SUS-006` | A concurrent resume changed the worker before resume completed. |
| `HARN-SUS-007` | `conditions.trigger` could not be registered with the trigger dispatcher. |
| `HARN-SUS-008` | A timeout fired or was configured with an unsupported `on_timeout` action. |
| `HARN-SUS-009` | Resume input failed `agent_loop` input validation. |
| `HARN-SUS-010` | A worker closed while suspended rejected a later resume. |

The diagnostic catalog at [Diagnostic codes catalog](./diagnostics.md) is
authoritative; the table above lists only the `HARN-SUS-*` namespace.

## Gotchas

- **Suspend is cooperative, not preemptive.** `suspend_agent(...)` flips
  a flag; the worker exits the loop frame at the next turn boundary, not
  inside a tool call or LLM request. Workers that never return to a turn
  boundary will never observe the request. Cap long-running tools with
  `tool_call_timeout` (see [Long-running tools](./long-running-tools.md)).
- **Conditions are optional.** A bare `agent_await_resumption("waiting")`
  parks the worker open — only the parent agent or an operator can wake
  it. Add `conditions` only when the agent itself knows what should fire
  the resume.
- **Snapshots survive process restart.** Both the snapshot file
  (`.harn/workers/worker_*.json`) and any registered trigger conditions
  are durable. A fresh process can load the snapshot with
  `resume_agent(snapshot)` or `harn run --resume <path>` and continue.
  When a cloud session is bound, `ResumeBy.cloud_harness` keeps the
  registration with the cloud session so the resume crosses machines.
- **Double-resume is detected.** Concurrent `resume_agent` calls on the
  same handle raise `HARN-SUS-006`; the second caller observes the race
  and can retry against the now-running handle. See the
  `suspend_double_resume_race` conformance test for the exact contract.
- **Closing a suspended worker is terminal.** `close_agent(handle)` on a
  suspended worker marks the snapshot rejected; a later `resume_agent`
  raises `HARN-SUS-010`. Closing is the correct call when the operator
  has decided the work is no longer relevant.
- **Graceful stop is a handoff, not a resume checkpoint.** Use
  `agent_stop(handle, {graceful: true})` when the parent should continue the
  work itself. Use `suspend_agent`/`resume_agent` when the same child should
  continue later.

## See also

- [LLM quick reference](./docs/llm/harn-quickref.md) — "Agent lifecycle:
  pause, resume, self-park" section.
- [Agent loops](./llm/agent_loop.md) — full `agent_loop` reference.
- [Daemon stdlib](./stdlib/daemon.md) — daemon-mode wrappers built on
  suspend/resume.
- [Agent channels](./agent-channels.md) — the durable pub/sub primitive
  that pairs naturally with `agent_await_resumption(reason,
  conditions.trigger: {kind: "channel.emit", ...})`.
- [ACP `session/suspend` RFC](./protocol-contributions/acp-session-suspend.md)
  and [A2A `TaskState.PAUSED` RFC](./protocol-contributions/a2a-paused-state.md)
  — upstream protocol companions.
- Conformance fixtures under `conformance/tests/agents/` — `suspend_*.harn`,
  `agent_await_resumption.harn`, `agent_top_level_await_resumption_cli.harn`,
  and `resume_by_*.harn` exercise every contract documented here.
