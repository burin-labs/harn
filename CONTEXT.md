# Harn

Harn is the provider-neutral execution language and runtime for durable,
inspectable agent workflows.

## Execution evidence

**Execution**:
One top-level invocation of a compiled Harn program, including all child tasks,
agent turns, effects, and its terminal outcome.
_Avoid_: Workflow run, session, trace

**Execution fact**:
An ordered, typed, redacted statement about something that occurred during one
execution and can be durably replayed.
_Avoid_: Telemetry event, log line, span

**Execution evidence**:
The complete durable fact stream for one execution plus the identity and
integrity metadata needed to verify its order and terminal state.
_Avoid_: Observability data, trace, event dump

**Projection**:
A replaceable view derived from execution evidence, such as a run record,
OpenTelemetry trace, CLI event stream, Replay Lab view, or host presentation.
_Avoid_: Source of truth, duplicate record

**Run record**:
The materialized Harn product view of one execution, derived from its execution
evidence for inspection, replay, evaluation, and export.
_Avoid_: Event log, workflow-only record

**Flight recording**:
An opt-in, bounded sequence of source locations and control-flow outcomes that
shows the exact code path taken by an execution without recording values by default.
_Avoid_: Opcode trace, debug log, always-on span

## Conversation history

**Conversation message**:
A provider-neutral durable turn. Native assistant calls use `tool_calls` with
`id`, `name`, and `arguments`; native results use `tool_result` with the matching
`tool_call_id`. Provider adapters project these facts onto their wire formats.
_Avoid_: Provider message, Anthropic block, OpenAI message

**Provider continuation**:
Opaque provider-bound state required to continue a prior model turn. It is kept
apart from conversation content and returned only to the provider that created
it.
_Avoid_: Reasoning text, message block, transcript content

## External actions

**External action**:
A consequential effect in a provider or account outside the current Harn workspace.
_Avoid_: Tool call, transaction, side effect

**Action intent**:
An immutable, normalized proposal whose fingerprint covers the exact actor,
provider, capability, environment, payload, and external spend.
_Avoid_: Request, plan, tool arguments

**Action grant**:
A time-bounded authorization tied to exactly one action-intent fingerprint and its external-spend ceiling.
_Avoid_: Approval, permission

**Action receipt**:
A durable provider-neutral record of whether an external action was confirmed, denied, not dispatched, or left indeterminate.
_Avoid_: Result, response, log

**Reconciliation**:
A read-only provider query that resolves an indeterminate action receipt without dispatching the action again.
_Avoid_: Retry, recovery

## Run authority

**Prepared run**:
A run whose declared requirements have been reconciled with host facts, policy,
provenance, budgets, and approval availability before execution can begin.
_Avoid_: Preflight, launch config

**Authority requirement**:
A value-free declaration of one filesystem, process, network, secret-consumer,
environment, host, MCP, budget, provenance, or startup need.
_Avoid_: Permission string, raw secret

**Authority lease**:
A time-bounded, fingerprinted authorization to execute exactly one prepared run within its granted requirements.
_Avoid_: Session grant, approval receipt

**Authority delta**:
A typed request bound to a parent authority lease and intersected with its
ceiling, without mutating or broadening the parent lease.
_Avoid_: Escalation flag, policy exception

**Toolchain probe**:
An exact post-readiness inquiry that discovers toolchain read roots within a
previously reviewed authority ceiling.
_Avoid_: Preflight command, ambient path scan

**Platform identity broker**:
A host-side authority that exchanges a value-free identity reference for a
short-lived handle bound to one provider, audience, tenant, and consumer.
_Avoid_: Credential fallback, profile chain

**Opaque identity handle**:
A non-transferable, process-local capability to use one brokered platform
identity through its declared consumer.
_Avoid_: Token, secret value, bearer credential

**Authority receipt**:
A durable, non-authorizing record of requested, granted, used, denied, and unused run authority and its decider.
_Avoid_: Authority lease, log line
