# Glossary

One-line definitions for every term Harn uses to describe a conversation, its
parts, and its containers. Where two terms in Harn mean the same thing, the
preferred one is marked; the others are alias-only.

For SOTA cross-references (what LangGraph or OpenAI or ACP calls the same idea),
see [Coming from elsewhere](./sota-comparison.md).

## Conversation units

**LLM call.** One request to a language model. Smallest unit. Produced by
`harness.llm.call`.

**Token reference.** One token ID paired with its exact tokenizer vocabulary.
Harn's `TokenRef` prevents a bare integer from crossing into a model that gives
that integer another meaning. See [Exact token references](../llm/tokenizer.md).

**Tool call.** The model's request to invoke a named tool, with arguments. Lives
inside an iteration; an iteration can contain several. Executed by
`agent_dispatch_tool_call` or by `agent_loop` automatically.

**Iteration.** One model round-trip inside an agent loop: prompt-out,
response-in, optional tool dispatch. Counted in `result.llm.iterations`.
*Preferred name.*

**Round-trip.** Alias for **iteration**. Used in prose; prefer "iteration" in
field names and code.

**Turn.** Overloaded. Prefer **iteration** for one model round-trip and
**prompt turn** for the outer user-message cycle. See
[Coming from elsewhere](./sota-comparison.md) for ACP terminology.

**Prompt turn.** The outer cycle: one user message → final agent response,
terminated by a `stop_reason`. Maps directly to ACP's `prompt_turn` and to one
invocation of `agent_loop`.

**Agent loop.** A function that runs iterations until completion. The
`agent_loop` stdlib entrypoint owns this lifecycle. Status outcomes include
`done`, `stuck`, `suspended`,
`budget_exhausted`, `provider_error`, `idle`, `watchdog`, `failed`.

**Daemon loop.** An agent loop that idles waiting for wake sources (triggers,
timers) instead of returning when no work is pending. Same primitive, different
terminal conditions.

## Containers and graphs

**VM (virtual machine).** The Harn interpreter state for one running script or
child task. Most user-facing docs say "interpreter instance" or "child task";
runtime and ADR pages often use VM because they describe implementation
boundaries.

**Portable kernel.** The authority-free Harn compiler and deterministic
execution state machine shared by native and browser hosts. It accepts a
versioned program artifact and returns completed, suspended, or failed.

**Program artifact.** An immutable, versioned encoding of checked Harn
bytecode plus the metadata needed by the portable kernel. It is data, not a
serialized Rust object or a grant of host authority.

**Linked program.** The closed native execution artifact inside a schema-v3
`.harnpack`. It contains the entry bytecode and only the reachable module
symbols needed by that exact source graph. It is separate from the
authority-free portable-kernel program artifact.

**Capability request.** A typed request emitted when portable execution needs
host-owned authority. The host may deny it or resume the authenticated
snapshot with a matching typed result.

**Run authority posture.** The typed combination of run interactivity,
approval availability, and workspace trust used to construct one run approval
policy before execution.

**Host-materialized workspace.** An isolated workspace created by the host for
one run. Its trust is a run fact and is not written to a durable per-path trust
store.

**Child VM.** The isolated interpreter instance created for a `spawn` or
`parallel` child task. Captured values are copied into it. Explicit shared
handles such as channels, shared cells/maps, mailboxes, and sync permits are
the way child tasks coordinate with siblings or the parent.

**Stage.** One node in a workflow graph. Kinds: `stage`, `verify`, `join`,
`condition`, `fork`, `map`, `reduce`, `subagent`, `escalation`.

**Workflow.** A typed, inspectable, replayable graph of stages with edges and
per-node policies. Executed by `workflow_execute`. Lives above `agent_loop` when
orchestration structure matters. See the
[workflow runtime](../workflow-runtime.md).

**Pipeline.** The `pipeline` language keyword: a named, callable, function-like
composition serving as the top-level entrypoint of a `.harn` program, with
lifecycle callbacks. The container in which agents, workers, and workflows run.
Not itself agentic — and not the stage-graph runtime, which is a **workflow**
(above). See the [pipeline lifecycle](../pipeline-lifecycle.md).

**Workflow session.** The durable execution record of one `workflow_execute`
run. Holds artifacts, per-stage results, and the replay trace.

## Durable state

**Execution.** One top-level Harn program invocation and every child VM it
creates. Its durable `hxe-...` identity is shared by local spans, run records,
flight recordings, OpenTelemetry, and host projections.

**Execution evidence.** The facts and artifacts Harn records about an
execution. A run record is the durable index. Spans and an optional flight
recording are projections or linked artifacts, not separate execution owners.

**Run record.** The durable JSON index for one execution or workflow session.
It carries lifecycle state, evidence identity, spans, artifacts, transcript
pointers, and replay inputs that the run produced.

**Flight recording.** An opt-in, bounded record of the exact VM instructions
and source locations an execution reached. It omits runtime values, arguments,
results, and stack contents. See [Debugging agent runs](../debugging.md#record-the-exact-code-path).

**Model job.** One finite, asynchronous model request with a closed lifecycle:
`queued`, `running`, `succeeded`, `failed`, or `canceled`. Harn owns its events,
receipt, output storage, and replay; a backend owns provider translation. See
[Why Harn has model jobs](./model-jobs.md).

**Media asset.** Model output bytes stored under their SHA-256 digest. A media
asset has a portable `asset://sha256/...` identity, a verified MIME type, and a
current local path. See the [model-job reference](../stdlib/model-jobs.md).

**Session.** The first-class VM resource that owns a transcript, subscribers,
parent/child lineage, a pinned system prompt, and a pinned model. Created by
`agent_session_open`. Outlives any single agent loop. Its `session_id`
identifies the transcript owner; a `run_id` identifies one exact invocation
within that history.

**Transcript.** The structured `{messages, events, assets}` document that hangs
off a session. `messages` are durable conversational turns; `events` are an
audit trail; `assets` are large or non-text payloads.

**Transcript event.** One entry in the `events` log. Includes `iteration_start`,
`iteration_end`, tool dispatch events, reminder events, and lifecycle events.

**Run report.** The versioned JSON view produced by
`harn runs report` or the `harn.run.report` MCP tool. It correlates a root run
with delegated child runs, timelines, trace spans, and verified transcript
pointers, then reports structural checks without changing the source data.

**Run review.** The versioned model assessment produced by `harn runs review`
or `harn.run.review` from one validated run report. Harn can build that report
in memory from a root run record. The review binds its verdict and evidence-addressed
findings to the report, rubric, and resolved model route. It does not replace
the run report's deterministic checks or read source artifacts itself.

**Snapshot.** A frozen, serializable copy of a session or worker state, used for
resume-after-suspend and for replay.

**System reminder.** A typed, turn-boundary injection into the transcript.
Carries a `mode` (`interrupt_immediate`, `finish_step`, `audit_only`),
a `role_hint`, optional `dedupe_key`, and optional TTL. See [System
reminders](../system-reminders.md).

## Hypotheses and evidence

**Hypothesis.** A versioned, testable claim with an owner, provenance, and
explicit evidence lane. It is not a fact or a mutable confidence label.

**Evidence policy.** The typed contract that selects the registered inference
mode, practical threshold, evidence ladder, claim ceiling, and explicit gate
promotion. The experiment registration owns assignment and statistical
decisions; the design budget owns execution ceilings.

**Experiment plan.** The deterministic compilation of a hypothesis and evidence
policy into an existing experiment registration. It is data, not model-authored
Harn source, an executable workflow, or a grant of host authority. A registered
host adapter must separately enforce approval, capability, and resource
ceilings.

**Hypothesis-event authority.** A non-serializable proof minted by a registered
native adapter and bound to one event fingerprint, plan fingerprint, aggregate,
run, and authority kind. It authorizes one specialized append to the reserved
hypothesis topic. Serialized event payloads and audit headers are provenance,
not authority.

**Observation.** One immutable, assignment-bound measurement admitted by an
evidence policy. A revised value is a new corrective event, never an in-place
edit of accumulated evidence.

**Decision.** A typed statistical result derived from registered evidence and a
frozen policy. It does not itself mutate a product default.

**Hypothesis workflow.** A read-first state machine over the canonical
hypothesis ledger. It inspects current state; controls start, pause, resume, and
stand-down transitions; and advances one Harn-randomized case/trial block at a
time through a registered native adapter. Harn owns assignment, admission,
stopping, and decision. Without the adapter, a mutating request returns
`adapter_unavailable` and records no lifecycle event.

**Promotion proposal.** A decision-bound request for a host-owned product
change. Approval and application are separate events with separate receipts.

## Delegation

**Worker.** An agent running in its own execution context with its own
transcript and loop. Spawned by `spawn_agent`; can be suspended, snapshotted,
and resumed. The unit of parallelism and of multi-agent orchestration.

**Subagent.** A worker in a workflow context. The `subagent` node kind delegates
a stage to a child agent.

**Persona.** A typed multi-stage agent identity with handoff policies, profile
bulletins, and per-stage tool scoping. Built on top of agent loops and sessions.

**Skill.** A bundle of metadata, system-prompt fragment, scoped tools, and
lifecycle hooks. Passed to `agent_loop` via the `skills:` option to match,
activate, scope, and deactivate across iterations.

## Steering and lifecycle

**Suspend.** Cooperatively pause a worker at the next iteration boundary.
Persists a resumable snapshot. Not a sleep — the runtime honors the boundary and
emits a lifecycle event.

**Resume.** Wake a suspended worker, optionally with new input.

**Self-park.** A worker pausing itself from inside the loop via
`agent_await_resumption(reason, conditions, resume_by)`. The model decides to
wait for something.

**Steering.** Any out-of-band influence on a running agent: injecting a system
reminder, queuing a user message, revoking a pending injection, cancelling an
in-flight tool call. See [Steering seams](./steering-seams.md).

**Inject mode.** The bridge-injection delivery variant for queued user messages
and system reminders. Three runtime values: `interrupt_immediate` (drain at the
next safe seam, including `pre_tool_dispatch` — the model's pending tool batch
is skipped when one arrives there), `finish_step` (drain at the next iteration
boundary), `audit_only` (drain at loop exit and append to the transcript; the
model never sees these — use `finish_step` if the model must react before the
agent terminates). The full seam catalog lives in
[Steering seams](./steering-seams.md).

**Checkpoint.** A safe point in the loop body where the runtime checks for
pending steering injections. Every drain in the agent loop and the daemon
idle path routes through the typed `agent_stage` seam; observers subscribe via
`harness.agent.register_checkpoint_hook(kinds, handler)`. See
[Steering seams](./steering-seams.md) for the canonical catalog.

## Things Harn doesn't use as nouns

**Thread.** Not a Harn term. The role thread plays in Mastra and LangGraph is
filled by **session** here. If you arrive from those systems, read `thread` as
`session`.

**Step.** Used informally in prose; the formal noun for the same concept is
**stage** in workflows and **iteration** in agent loops. The Inngest-style
`step.run` memoization barrier exists as a stdlib namespace for durable replay
of completed handler results.

**Run.** Used colloquially for "one invocation of a pipeline or workflow." Not a
first-class noun in the language. Persisted runtime records still use a
`run_id` as the stable identity of one exact invocation; do not substitute a
session ID for it.

**Phase.** Appears around pipeline lifecycle callbacks but is not a
conversational-unit noun.

## Where each concept's authoritative reference lives

| Concept | Reference page |
|---|---|
| `harness.llm.call`, `harness.llm.call_structured`, `harness.llm.completion` | [LLM calls](../llm/llm_call.md) |
| `TokenRef`, `tokenize`, `detokenize`, `logit_bias` | [Exact token references](../llm/tokenizer.md) |
| `agent_loop`, `AgentSpec`, profiles, `turn_end_condition` | [Agent loops](../llm/agent_loop.md) |
| Tools, Tool Vault, MCP server tools | [LLM tools](../llm/tools.md) |
| Sessions, fork, reset, compact, snapshot | [Sessions](../sessions.md) |
| Transcripts, events, assets | [Transcript architecture](../transcript-architecture.md) |
| Workers, suspend, resume, self-park | [Agent lifecycle](../agent-lifecycle.md) |
| Workflows, graphs, stages | [Workflow runtime](../workflow-runtime.md) |
| Pipelines, harness, lifecycle callbacks | [Pipeline lifecycle](../pipeline-lifecycle.md) |
| Portable artifacts, capability requests, execute/resume | [Portable kernel contract](../portable-kernel-reference.md) |
| System reminders, inject modes | [System reminders](../system-reminders.md) |
| Skills | [Skills](../skills.md) |
| Model jobs, receipts, media assets | [Model-job reference](../stdlib/model-jobs.md) |
| Personas | [Personas](../personas.md) |
| Daemon loops | [Daemon stdlib](../stdlib/daemon.md) |
| Hypotheses, evidence policy, experiment plans, decisions | [ADR 0007](../adr/0007-hypothesis-compiler-ownership.md) |
