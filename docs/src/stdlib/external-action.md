# External actions

`std/external_action` binds consequential provider effects to exact
authorization and durable receipts. It is the shared lifecycle for purchases,
messages, calendar writes, trades, infrastructure changes, and similar actions;
provider packages still own their API transport and Burin or another host still
owns the effective approval policy.

The lifecycle is:

1. Normalize untrusted action data with `external_action_intent(...)`.
2. Let the host evaluate user and workspace policy. Normalize any organization
   restrictions with `external_action_managed_policy(...)`.
3. Encode the effective decision with `external_action_grant(...)`.
4. Call `external_action_execute(...)` with a provider adapter.
5. If the receipt is `reconciliation_required`, query the provider with
   `external_action_reconcile(...)`; never repeat dispatch speculatively.
6. Project the intent, policy chain, decision, disclosure classes, dispatch,
   reconciliation, and receipt with `external_action_activity(...)`.

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
  payload: {offer_id: offer_id},
  protected_disclosure: {
    field_classes: ["legal_identity", "birth_date", "contact_details"],
    recipient: "Duffel",
    purpose: "Create the exact approved order.",
    source: "user_profile",
    require_reauthentication: true,
  },
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

const managed = external_action_managed_policy({
  automatic_approval_forbidden: true,
  live_actions_forbidden: true,
  minimum_authentication_assurance: "biometric",
  max_external_spend: [{currency: "USD", amount_minor: 50000}],
  allowed_providers: ["duffel"],
  allowed_capabilities: ["flights.book"],
  allowed_environments: ["test"],
})

// The product host owns this resolver. It returns requested values only after
// the exact grant and managed policy pass. The values exist only for this
// adapter call; Harn checkpoints and receipts retain metadata, never values.
const receipt = external_action_execute(
  harness,
  intent,
  grant,
  duffel_adapter,
  managed,
  protected_profile_resolver,
)
```

## Guarantees

- The fingerprint covers actor, provider, capability, operation, environment,
  payload, protected-disclosure plan, and external spend. Changing any of them
  invalidates the grant.
- A protected-disclosure plan contains only field classes, recipient, purpose,
  source, and reauthentication policy. Personal values must not be placed in
  the intent payload.
- Grant and managed-policy checks run before the disclosure resolver. Missing,
  refused, malformed, or mismatched disclosure produces a typed denial and
  zero provider dispatches.
- Reauthentication is separate from action approval. When a plan requires it,
  the protected-profile host must report biometric or managed assurance from
  that disclosure attempt; the receipt records that actual assurance.
- Requested personal values are validated against a closed field-class
  contract immediately before dispatch. The adapter receives them transiently.
  Checkpoints, receipt events, replay state, and receipts retain only the
  value-free disclosure record.
- Legal identity can include the closed `m` or `f` passenger marker required by
  current airline order APIs. It remains inside the protected disclosure and
  never moves into an intent, transcript, receipt, or activity record.
- Model/API inference cost is not part of `external_spend`; hosts can budget
  the two independently.
- Managed policy is a restriction layer, not a grant. It can disable actions,
  require manual approval or stronger authentication, forbid live effects,
  reduce a currency limit, or restrict providers, capabilities, and
  environments. It cannot create authority that the exact grant does not have.
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
- `external_action_activity(...)` produces one intent-keyed, value-free
  snapshot. It records model, agent, session, policy-layer decisions, grant
  expiry, disclosure classes, dispatch, reconciliation, and the canonical
  receipt. Payloads, protected values, credentials, and reusable grants are
  structurally absent. Product and Cloud stores update this record instead of
  maintaining a second approval history.

Use `external_action_fake_adapter(...)` in deterministic tests. A production
connector and the fake both implement the same two-method adapter seam:
`dispatch` and `reconcile`.

The unified operational redactor treats `protected_disclosure` and
`protected_values` containers as sensitive. Connector adapters must also apply
their manifest redaction rules when they translate protected fields into a
provider-specific request shape.
