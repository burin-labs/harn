# Soft-landing cut rules

Two modules decide when a run stops and what its stop proves.

| Module | Owns |
| --- | --- |
| `std/agent/run_meter` | The typed run meter: what the run has spent, and what the next call is projected to cost. |
| `std/agent/cut_landing` | The closed predicate grammar over meter fields, and the latched `running -> landing -> stopped` machine with its grace envelope, admission, and receipts. |

They are pure. No clock, no counters, no I/O. The caller supplies observations
and acts on the decision.

## Actual usage and a projected bound are different numbers

Every meter field carries a `role`. An `actual` field is accounting truth: what
the run has already consumed. An `admission` field is a bound priced for the
*next* model call, and it exists only to answer whether that call may start.

`run_meter_charge` refuses an `admission` field, so a projection can never be
added to actual spend. That is what lets a run stopped by a conservative
estimate still report what it really cost.

```harn
import {
  run_meter_charge,
  run_meter_exact,
  run_meter_new,
  run_meter_project_next_call,
} from "std/agent/run_meter"

const empty = run_meter_new()
const started = unwrap(run_meter_exact(empty, "actual_cost_usd", 0.84))
const metered = unwrap(run_meter_project_next_call(started, 0.90))
// Err: a projection is not spend.
const projected = "projected_next_call_cost_usd"
const refused = run_meter_charge(metered, projected, 0.90)
```

## An unmeasured field is not a zero

An observation is `exact`, `bounded`, or `unavailable`. A field the provider
never reported is `unavailable`, and a field never written is `unavailable`
with a different reason. Neither is a measured zero.

| Observation | `at_least(threshold)` answers |
| --- | --- |
| `exact(v)` | `matched` when `v >= threshold`, else `not_matched` |
| `bounded(lo, hi)` | `matched` when `lo >= threshold`; `not_matched` when `hi < threshold`; otherwise `indeterminate` |
| `unavailable(why)` | `indeterminate` |

`all` and `any` combine three-valued. One `not_matched` child settles an `all`;
one `matched` child settles an `any`; otherwise an `indeterminate` child makes
the group `indeterminate`. Both reject an empty group, because an empty `all`
is vacuously true and an empty `any` is vacuously false, and each is a silent
way for a rule that names no condition to look decided.

## Registered meter fields

| Field | Unit | Role |
| --- | --- | --- |
| `actual_cost_usd` | usd | actual |
| `projected_next_call_cost_usd` | usd | admission |
| `input_tokens` | tokens | actual |
| `output_tokens` | tokens | actual |
| `cache_read_tokens` | tokens | actual |
| `cache_write_tokens` | tokens | actual |
| `projected_next_call_input_tokens` | tokens | admission |
| `projected_next_call_output_tokens` | tokens | admission |
| `wall_ms` | ms | actual |
| `turns` | count | actual |
| `model_requests` | count | actual |
| `tool_calls` | count | actual |
| `provider_errors` | count | actual |
| `verifier_completions` | count | actual |

Reading or writing a name outside this registry is refused rather than
tolerated. `run_meter_fields()` returns the live list and
`run_meter_registry_digest()` pins it into a receipt.

## Writing a policy

A policy is a list of named rules. Each rule is a predicate and a typed effect.

```harn
import {
  cut_rules_tick,
  cut_state_new,
  CutRulePolicy,
} from "std/agent/cut_landing"

// Spend up to $1.50; or keep running up to 30 more minutes past
// that, to a $3.00 hard ceiling; and at the soft boundary finish
// the current turn.
const policy: CutRulePolicy = {
  rules: [
    {
      name: "hard_ceiling",
      when: {op: "at_least", field: "actual_cost_usd", threshold: 3.0},
      effect: {effect: "stop"},
    },
    {
      name: "soft_cost_cap",
      when: {op: "at_least", field: "actual_cost_usd", threshold: 1.5},
      effect: {
        effect: "land",
        landing: "finish_turn",
        grace: {extra_cost_usd: 0.25, extra_wall_ms: 1800000.0},
      },
    },
  ],
}

const tick = cut_rules_tick(policy, meter, cut_state_new())
```

A `land` effect must carry its own grace envelope, so a soft cap cannot latch
without a bound on what follows it. `cut_rules_validate` rejects a policy with
an empty rule set, a duplicate rule name, a malformed predicate, or a landing
whose envelope bounds nothing.

## The latch

`cut_rules_tick` evaluates every rule and advances one step.

| Phase | Meaning |
| --- | --- |
| `running` | No boundary crossed. |
| `landing` | A soft boundary latched. The run may finish the current turn or task inside the grace envelope. |
| `stopped` | Terminal. |

Selection is by effect strength, not rule order alone: a matched `stop` beats a
matched `land` on the same tick. Among equally strong matches the first in
policy order wins.

The latch is one-way. A later tick whose soft predicate no longer matches keeps
the run in `landing`, and a landing may tighten from `finish_task` to
`finish_turn` but never loosen.

The grace envelope is measured from a snapshot taken when the landing latched,
so it is an increment on what was already spent, not a new absolute cap. An
expired bound forces an immediate stop and names which bound expired. A bound
that is armed but cannot be measured records `grace_unmeasurable`, which keeps
the landing open for a terminal emit while denying further model calls.

## Admission and the terminal tool

`cut_action_allowed(state, meter, action)` answers one guarded action.

- While `running`, both actions are admitted.
- While `landing`, a `model_call` is admitted only when its projected upper
  bound fits the cost grace that remains. A denial reports the projection and
  the remaining grace as two numbers.
- While `landing`, a `terminal_tool` is always admitted. This is the generic
  capability a mode's final receipt emit consumes to force one last write at
  the boundary.
- While `stopped`, both are denied.

`cut_terminal_emit_due(state)` is true exactly while a landing still owes that
emit; `cut_state_record_terminal_emit` clears it.

## Receipts and terminal evidence

Every tick returns a `harn.cut_rules_tick.v1` receipt: total and evaluated rule
counts, matched and indeterminate rule names, the meter, registry, and rules
digests, every field read with its provenance, the selected effect and rule,
the state transition, actual cumulative cost beside the next-call projection,
and the accumulated cause chain.

A tick on a stopped run reports `evaluated_rules: 0` against a nonzero
`total_rules`, so a run that stopped is distinguishable from a rule set that
never ran.

The cause chain is append-only across ticks, with typed steps
`rule_evaluated`, `effect_selected`, `landing_chosen`, `admission_denied`,
`grace_expired`, `grace_unmeasurable`, and `terminated`.

`cut_terminal_evidence(state, meter)` reports `running`, `completed`, or
`forced_stop`. `completed` is reachable only through `cut_state_complete`,
which refuses unless the meter carries an exact, non-zero
`verifier_completions` count. A forced cut therefore cannot report completion.

See [Pace cut rules](./cut-rules.md) for the wall-clock pacing decision that
shares this module family.
