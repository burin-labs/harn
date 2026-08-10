# External actions

`std/external_action` binds consequential provider effects to exact
authorization and durable receipts. It is the shared lifecycle for purchases,
messages, calendar writes, trades, infrastructure changes, and similar actions;
provider packages still own their API transport and Burin or another host still
owns the effective approval policy.

The lifecycle is:

1. Normalize untrusted action data with `external_action_intent(...)`.
2. Let the host evaluate user, workspace, and managed-organization policy.
3. Encode the effective decision with `external_action_grant(...)`.
4. Call `external_action_execute(...)` with a provider adapter.
5. If the receipt is `reconciliation_required`, query the provider with
   `external_action_reconcile(...)`; never repeat dispatch speculatively.

```harn,ignore
import {
  external_action_execute,
  external_action_grant,
  external_action_intent,
} from "std/external_action"

const intent = external_action_intent({
  actor: {kind: "user", id: user_id},
  provider: "duffel",
  capability: "flights.book",
  operation: "create_order",
  environment: "test",
  payload: {offer_id: offer_id, passengers: passengers},
  external_spend: {currency: "USD", amount_minor: 24567},
  display: {summary: "Book SEA to JFK for $245.67"},
})

// The host supplies these effective authorization facts after evaluating its
// local and managed policy. Harn does not infer them from prompt prose.
const grant = external_action_grant(intent, {
  authorized_by: {kind: "user", id: user_id},
  authorization_method: "manual",
  authentication_assurance: "biometric",
  issued_at_ms: harness.clock.now_ms(),
  expires_at_ms: harness.clock.now_ms() + 300000,
  max_external_spend: {currency: "USD", amount_minor: 24567},
})

const receipt = external_action_execute(harness, intent, grant, duffel_adapter)
```

## Guarantees

- The fingerprint covers actor, provider, capability, operation, environment,
  payload, and external spend. Changing any of them invalidates the grant.
- Model/API inference cost is not part of `external_spend`; hosts can budget
  the two independently.
- Dispatch is checkpointed by the exact intent fingerprint. Replaying the same
  action returns the original receipt without calling the adapter again.
- Once dispatch starts, thrown or malformed provider responses become an
  indeterminate receipt. They are never treated as proof of failure.
- Reconciliation has a separate read-only adapter method and a caller-supplied
  stable attempt ID. Replaying one polling attempt does not query twice; a later
  poll can use a new ID.
- Receipts accept bounded reference identifiers, provider action IDs, and
  machine error codes. They do not retain arbitrary provider response bodies or
  secret-bearing headers.

Use `external_action_fake_adapter(...)` in deterministic tests. A production
connector and the fake both implement the same two-method adapter seam:
`dispatch` and `reconcile`.
