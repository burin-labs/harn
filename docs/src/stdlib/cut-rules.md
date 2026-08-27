# Pace cut rules

`std/agent/cut_rules` decides whether a run should keep going, receive more
time, get a wrap-up check, or stop. It does not read a clock, store counters,
send feedback, or stop the loop. The caller supplies facts and acts on the
result.

## Decide from pace facts

```harn
import { pace_cut_rule_decision } from "std/agent/cut_rules"

const decision = pace_cut_rule_decision(
  {extend_max: 6, pace_check_max: 2},
  {
    armed_budget_ms: 60000,
    elapsed_ms: 42000,
    checkpoint_ms: 30000,
    expected_total_ms: 55000,
    made_progress: true,
    verifier_signature_unchanged: false,
    done: false,
    extends_used: 1,
    pace_checks_used: 0,
    env_blame_without_infra: false,
  },
)
harness.stdio.log(decision.action)
```

`pace_cut_rule_decision(policy: PaceCutRulePolicy, obs: PaceCutRuleObservation)
-> PaceCutRuleDecision` is a pure function. `PaceCutRulePolicy` has two optional
limits:

| Field | Default | Meaning |
| --- | ---: | --- |
| `extend_max` | `6` | Maximum silent budget extensions |
| `pace_check_max` | `2` | Maximum wrap-up checks before a cut |

The observation accepts these fields:

| Field | Meaning |
| --- | --- |
| `armed_budget_ms` | Current wall-clock budget. A missing or non-positive value makes the rule inactive. |
| `elapsed_ms` | Time used since the run started. |
| `checkpoint_ms` | Decision interval. Defaults to `armed_budget_ms`. |
| `expected_total_ms` | Estimated total time. Defaults to `armed_budget_ms`. |
| `made_progress` | Whether verification advanced since the last checkpoint. |
| `verifier_signature_unchanged` | Whether work landed without improving the verifier result. |
| `done` | Whether the run already satisfied its completion check. |
| `extends_used` | Silent extensions already granted. |
| `pace_checks_used` | Wrap-up checks already sent. |
| `env_blame_without_infra` | Whether the run blamed its environment without an infrastructure signal. |

## Decision values

| Action | Meaning | Extra fields |
| --- | --- | --- |
| `proceed` | Keep the current budget. | None |
| `extend` | Move the wall-clock deadline forward. | `new_budget_ms`, `reason` |
| `pace_check` | Ask the run to check its pace and wrap up. | `reason` |
| `cut` | Stop the run. | `reason` |

The `cut` reasons are `no_progress_at_checkpoint`, `no_verifier_progress`,
`env_blame_without_infra`, `extend_budget_exhausted`, and
`pace_check_budget_exhausted`.

`pace_cut_rule_action_of(decision)` maps these actions to the shared governor
vocabulary. `cut` becomes `abort`, `pace_check` becomes `warn`, and both
`extend` and `proceed` become `proceed`.

Use `pace_cut_rule_check_max_injections()` and
`pace_cut_rule_extend_max()` when another policy needs the default limits.

The caller owns the clock, stored counters, feedback, and loop actuation. See
[Host-supplied facts](./fact-intake-seams.md#pace_cut_rule_decision-a-smart-timeout)
for the boundary between those facts and this decision.
