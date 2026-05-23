# Glossary

One-line definitions for every term Harn uses to describe a conversation, its
parts, and its containers. Where two terms in Harn mean the same thing, the
preferred one is marked; the others are alias-only.

For SOTA cross-references (what LangGraph or OpenAI or ACP calls the same idea),
see [Coming from elsewhere](./sota-comparison.md).

## Conversation units

**LLM call.** One request to a language model. Smallest unit. Produced by
`llm_call`.

**Tool call.** The model's request to invoke a named tool, with arguments. Lives
inside an iteration; an iteration can contain several. Executed by
`agent_dispatch_tool_call` or by `agent_loop` automatically.

**Iteration.** One model round-trip inside an agent loop: prompt-out,
response-in, optional tool dispatch. Counted in `result.llm.iterations`.
*Preferred name.*

**Round-trip.** Alias for **iteration**. Used in prose; prefer "iteration" in
field names and code.

**Turn.** Overloaded — historically used for both inner (iteration) and outer
(`agent_turn` wrapper) units. Prefer **iteration** for the inner unit and
**prompt turn** for the outer unit. See [SOTA comparison](./sota-comparison.md)
for the ACP alignment that motivates this.

**Prompt turn.** The outer cycle: one user message → final agent response,
terminated by a `stop_reason`. Maps directly to ACP's `prompt_turn` and to one
invocation of `agent_turn`.

**Agent loop.** A function that runs iterations until completion. The body of
the `agent_loop` builtin. Status outcomes: `done`, `stuck`, `suspended`,
`budget_exhausted`, `provider_error`, `idle`, `watchdog`, `failed`.

**Agent turn (`agent_turn`).** A wrapper around `agent_loop` that adds a
completion judge and runs one prompt-turn-shaped invocation. Despite the name,
it can contain many iterations.

**Daemon loop.** An agent loop that idles waiting for wake sources (triggers,
timers) instead of returning when no work is pending. Same primitive, different
terminal conditions.

## Containers and graphs

**Stage.** One node in a workflow graph. Kinds: `stage`, `verify`, `join`,
`condition`, `fork`, `map`, `reduce`, `subagent`, `escalation`.

**Workflow.** A typed, inspectable, replayable graph of stages with edges and
per-node policies. Executed by `workflow_execute`. Lives above `agent_loop` when
orchestration structure matters.

**Pipeline.** The top-level `.harn` program with `fn main(harness)` and
lifecycle callbacks. The container in which agents, workers, and workflows run.
Not itself agentic.

**Workflow session.** The durable execution record of one `workflow_execute`
run. Holds artifacts, per-stage results, and the replay trace.

## Durable state

**Session.** The first-class VM resource that owns a transcript, subscribers,
parent/child lineage, a pinned system prompt, and a pinned model. Created by
`agent_session_open`. Outlives any single agent loop.

**Transcript.** The structured `{messages, events, assets}` document that hangs
off a session. `messages` are durable conversational turns; `events` are an
audit trail; `assets` are large or non-text payloads.

**Transcript event.** One entry in the `events` log. Includes `turn_start`,
`turn_end`, tool dispatch events, reminder events, and lifecycle events.

**Snapshot.** A frozen, serializable copy of a session or worker state, used for
resume-after-suspend and for replay.

**System reminder.** A typed, turn-boundary injection into the transcript.
Carries a `mode` (`interrupt_immediate`, `finish_step`, `wait_for_completion`),
a `role_hint`, optional `dedupe_key`, and optional TTL. See [System
reminders](../system-reminders.md).

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

**Inject mode.** The bridge-injection variant for system reminders. Three
values: `interrupt_immediate` (drain at the next safe seam, currently next
iteration boundary; see issue
[#2211](https://github.com/burin-labs/harn/issues/2211) for the in-progress
upgrade to true mid-step preemption), `finish_step` (drain at the next iteration
boundary), `wait_for_completion` (drain at loop exit; note
[#2212](https://github.com/burin-labs/harn/issues/2212) — these are not yet
rendered to the model).

**Checkpoint.** A safe point in the loop body where the runtime checks for
pending steering injections. Today: turn-start, turn-end, post-compact,
daemon-idle. Tracked for unification in
[#2211](https://github.com/burin-labs/harn/issues/2211).

## Things Harn doesn't use as nouns

**Thread.** Not a Harn term. The role thread plays in Mastra and LangGraph is
filled by **session** here. If you arrive from those systems, read `thread` as
`session`.

**Step.** Used informally in prose; the formal noun for the same concept is
**stage** in workflows and **iteration** in agent loops. The Inngest sense of
`step.run` as a memoization barrier is a deliberate non-feature today — tracked
at [#2217](https://github.com/burin-labs/harn/issues/2217).

**Run.** Used colloquially for "one invocation of a pipeline or workflow." Not a
first-class noun in the language.

**Phase.** Appears around pipeline lifecycle callbacks but is not a
conversational-unit noun.

## Where each concept's authoritative reference lives

| Concept | Reference page |
|---|---|
| `llm_call`, `llm_call_structured`, `llm_completion` | [LLM calls](../llm/llm_call.md) |
| `agent_loop`, `agent_turn`, profiles, `done_judge` | [Agent loops](../llm/agent_loop.md) |
| Tools, Tool Vault, MCP server tools | [LLM tools](../llm/tools.md) |
| Sessions, fork, reset, compact, snapshot | [Sessions](../sessions.md) |
| Transcripts, events, assets | [Transcript architecture](../transcript-architecture.md) |
| Workers, suspend, resume, self-park | [Agent lifecycle](../agent-lifecycle.md) |
| Workflows, graphs, stages | [Workflow runtime](../workflow-runtime.md) |
| Pipelines, harness, lifecycle callbacks | [Pipeline lifecycle](../pipeline-lifecycle.md) |
| System reminders, inject modes | [System reminders](../system-reminders.md) |
| Skills | [Skills](../skills.md) |
| Personas | [Personas](../personas.md) |
| Daemon loops | [Daemon stdlib](../stdlib/daemon.md) |
