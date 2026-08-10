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
