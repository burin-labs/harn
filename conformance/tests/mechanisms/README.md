# Mechanism contracts (the onramp tier)

The **first required rung below the N≥5 convergence gauntlet.** A mechanism
contract is a _manufactured mini-eval_: a deterministic fixture, driven by a
mock provider scripted to emit an exact event sequence, that proves a harness
feature **engages correctly in isolation** before any convergence measurement.

It exists because the team has shipped features that went straight from
"implemented" to "measure on the full gauntlet" and only discovered _via a full
track day_ that the mechanism never attached: the smart-escalation ladder
masqueraded a provider failure as success (harn #3543) and cut a converging
`swift-feat` run at 3 strikes with ~85% of its budget unused. Those are
"does it attach?" failures — the gauntlet is the wrong, slow, expensive,
confounded tool for them.

## The contract

Every new **termination / escalation / judge / guard / routing** mechanism
declares a contract = **{trigger, effect, negative}**:

- **trigger** — the condition it claims to handle, _manufactured_ by a mock
  provider scripted to emit the exact event sequence;
- **effect** — the observable `AgentEvent` / status it must produce;
- **negative** — the case where it must **not** fire.

A fixture drives `agent_loop` through the mock and pins each clause as a
deterministic golden line. No real model, no cloud, milliseconds,
exact-input → exact-output. This is **purely fitness-for-purpose** — it does not
try to prove the feature improves convergence or doesn't regress other
qualities. That stays the gauntlet's job.

## Relationship to the meter gate

A GREEN contract is a **precondition to starting a meter run, never a
replacement** for it. The N≥5 paired-CI meter (`docs/eval/meter-stick.md`) still
owns every convergence claim. The onramp just refuses to spend gauntlet hours on
a mechanism that has not been shown to engage.

## Authoring a contract

1. Add `conformance/tests/mechanisms/<name>.contract.harn` with a
   `pipeline default()` that drives `agent_loop` and prints one golden line per
   clause via the `contract_effect` / `contract_negative` helpers in
   `lib/mechanism_contract.harn`.
2. Capture the golden into `<name>.contract.expected`.
3. Run `make mechanism-contracts` (a fast filter over `*.contract.harn`).
   `make conformance` already covers this directory in CI.

### Fixture-authoring rule (read this)

A multi-turn mock **must not** hold per-turn state in a closure `var` counter.
The agent loop runs the `llm_caller` inside a nested execution descent with an
isolated environment, so outer-scope `var` mutations do not persist across
turns — a counter silently replays turn 0 forever and the run hits its budget
instead of the behavior you scripted (a GREEN-looking but meaningless test).
Script a turn sequence with either:

- **`shared_cell` / `shared_snapshot` / `shared_set`** (scope `task_group`) —
  the canonical cross-descent state primitive (see
  `../agents/agent_loop_escalation_provider_error.harn`); or
- the harness-supplied **`call.turn.iteration`** index — a stateless key the
  loop passes to the caller each turn.

## Worked example

The escalation/termination contract is split across two files so neither
duplicates the other:

- **positive clauses** — escalate on genuine provider failure, **emit** the
  `provider_error` event (not silent, the #3543 fix), degrade back to the
  primary — are pinned by
  `../agents/agent_loop_escalation_provider_error.harn`;
- **negative clause** — the terminator must **not** fire while the failing
  error set strictly **shrinks** and budget remains (the `swift-feat`
  premature-cut regression) — is pinned by
  `escalation_progress_credit.contract.harn` here.

Together they state the full {trigger, effect, negative} for escalation.
