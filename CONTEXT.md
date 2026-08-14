# Harn

Harn is the provider-neutral execution language and runtime for durable,
inspectable agent workflows.

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

**Authority receipt**:
A durable, non-authorizing record of requested, granted, used, denied, and unused run authority and its decider.
_Avoid_: Authority lease, log line
