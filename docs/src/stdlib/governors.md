# Agent governors and detectors

`std/agent/governors` and the unified detector surface in `std/agent/stall`
provide the generic runtime guardrails an agent host would otherwise hand-roll
on top of `agent_loop`: a **pace/budget governor** that slows and stops a run as
it consumes its budget, and a **detector subsystem** that unifies loop,
no-progress, stuck-tool, and token-runaway detection.

Both speak ONE governance vocabulary, collapsed onto the three actions the live
post-turn seam can take:

| Action    | Meaning                                   | Verdict on the seam            |
|-----------|-------------------------------------------|--------------------------------|
| `proceed` | do nothing, keep the run going            | `nil`                          |
| `warn`    | inject a wrap-up reminder and continue    | `{message}`                    |
| `abort`   | stop the run before overrun               | `{stop: true, stop_reason}`    |

Governors and detectors are **values you compose into the existing
`post_turn_callback` seam** — never a new hook on `agent_loop`.

## Convergence guard

`convergence_guard_decision` is the Harn-owned policy core for known spiral
shapes that hosts can describe with typed facts. The first built-in shape is
`finalization_runaway_on_green`: required verification is already green, the
output contract has not failed, no post-green task diff landed, the run has
post-green churn, and the run starts spending turns on proof-only artifacts,
marker commands, policy-denied proof attempts, or verify-only retries.

```harn
import { convergence_guard_decision } from "std/agent/governors"

const decision = convergence_guard_decision(
  {},
  {
    required_verification_green: true,
    output_contract_passed: true,
    post_green_task_diff_count: 0,
    post_green_turns: 2,
    post_green_proof_artifact_writes: 1,
  },
)
log(decision.recovery.verb) // stop_on_green
```

The decision output is structured: `{shape, confidence, evidence, recovery,
receipt}`. Non-matches use `shape: "none"` and `recovery: nil`. `recovery.verb`
is declarative (`stop_on_green` for the deterministic green case), so hosts do
not need to concatenate prompt text or inspect English. The guard also consumes
compact stall facts such as `stall_warning`, `stall_no_net_progress`, and
`stall_patterns` as evidence of post-green churn; it does not duplicate the
stall detectors themselves.

## Pace / budget governor

A `GovernorPolicy` is a data row. It names a monotone consumption `signal`
(`iterations`, `tokens`, or `cost`), a `budget` ceiling in that signal's units,
and the consumption **fractions** at which the governor starts caring
(`checkpoint`), warns while still making progress (`over_estimate`), and
hard-stops regardless of progress (`hard`). Any successful tool call counts as
progress unless `progress_tools` restricts it.

```harn
import { governor_decision } from "std/agent/governors"

fn budget_action(consumed: float, made_progress: bool) -> string {
  const policy = {budget: 10.0, checkpoint: 1.0, over_estimate: 2.0, hard: 3.0, signal: "iterations"}
  const decision = governor_decision(
    policy,
    {ceiling: 10.0, consumed: consumed, made_progress: made_progress, signal: "iterations"},
  )
  return decision.action
}
```

`governor_decision` is a pure function; `governor_post_turn(policy)` wraps it
into a live `post_turn_callback` that reads the per-turn payload and steers.

```harn,ignore
import { governor_post_turn } from "std/agent/governors"

agent_loop(task, ctx, {
  post_turn_callback: governor_post_turn({budget: 40.0, signal: "iterations"}),
})
```

This generalizes burin-code's `pace-governor.harn`: `proceed`/`warn`/`abort`
correspond to burin's `extend`/`pace_check`/`cut`, and burin's write-progress
veto is preserved (progress vetoes the soft stop up to the `hard` fraction).

### Smart timeout: `governor_pace_decision`

`governor_decision` watches a consumption signal (iterations, tokens, cost)
against a budget. `governor_pace_decision` watches the **wall clock** instead,
and it does one thing the consumption governor can't: it lets a run that is
still making progress buy more time silently, so a job that's one turn from done
doesn't get killed by a fixed timeout, and a job stalled at the checkpoint gets
cut there instead of at the wall.

It is a pure function. You feed it a policy and an observation your host
assembles from the clock plus loop state, and it returns one of four actions.
Nothing about it touches `agent_loop` — you call it and act on the verdict.

```harn
import { governor_pace_decision } from "std/agent/governors"

const decision = governor_pace_decision(
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
log(decision.action)
```

**Policy.** Two bounded caps on the same `GovernorPolicy`: `extend_max`
(default 6) limits how many silent extensions a run can earn; `pace_check_max`
(default 2) limits how many recalibration nudges fire before the run is cut.

**Observation.** The host supplies the facts. `armed_budget_ms` (the wall budget
armed; `0` or absent means "not armed" and the governor always proceeds),
`elapsed_ms`, `checkpoint_ms` (defaults to the armed budget), `expected_total_ms`
(defaults to the armed budget), `made_progress`, `verifier_signature_unchanged`
(a write landed but the verifier failure signature did not improve — not
progress), `done`, `extends_used`, `pace_checks_used`, and
`env_blame_without_infra` (a typed classifier verdict the host supplies; the
pure governor never matches phrases itself).

**The verdict** is `{action, ...}`:

| Action | When | Extra fields |
| --- | --- | --- |
| `proceed` | budget inert, run `done`, or checkpoint not yet reached | — |
| `extend` | progressing and within the estimate — extend the wall silently | `new_budget_ms` (= `elapsed_ms + checkpoint_ms`), `reason` |
| `pace_check` | progressing but past the estimate — nudge it to wrap up | `reason` |
| `cut` | stop | `reason` |

`cut` reasons: `no_progress_at_checkpoint`, `no_verifier_progress`,
`env_blame_without_infra`, `extend_budget_exhausted` (past `extend_max`),
`pace_check_budget_exhausted` (past `pace_check_max`).

The host owns the actuation — arming the budget, tracking the extend/pace-check
counters across turns, injecting the pace-check nudge, and stopping the loop.
The governor only decides. See
[Host-supplied facts](./fact-intake-seams.md#governor_pace_decision-a-smart-timeout)
for the full contract on the facts your host feeds in.

## Unified detectors

A `DetectorSpec` is the single typed surface for all four detectors. The loop,
no-progress, and stuck rows lower onto the native `stall_diagnostics` config;
token-runaway is added as a `post_turn_callback` overlay that emits the same
`agent_loop_stall_warning` event.

```harn,ignore
import { with_governance } from "std/agent/governors"

const opts = with_governance(base_opts, {
  detectors: {
    loop: {repeat: 4, ping_pong_cycles: 6},
    no_progress: {messages: 3},
    stuck: {same_error: 3, same_diagnostic: 3},
    token_runaway: {median: 8000.0, stddev: 1500.0},
  },
  governor: {budget: 40.0, signal: "iterations"},
})
```

`token_runaway_decision` and `token_runaway_resolve_cap` are the pure
token-runaway core (input tokens vs `median + sigma*stddev`, hard overshoot
multiple `3.0`), mirroring burin-code's `token-runaway-guard.harn`.

## Composition and presets

`compose_post_turn([...])` chains several callbacks into one (the first stop
wins, warnings accumulate). `with_governance(opts, {governor, detectors})` folds
a governor and detector spec into an options dict, preserving any existing
`post_turn_callback` and `stall_diagnostics`. `agent_governed_preset(kind,
options, governance)` is `agent_preset` with governance folded in, so a durable
persona can carry its pace governor and detector rows by name.

## Live-shape assertion

Because a callback that keys on payload fields the live seam never populates is
dead on arrival, the governor emits an `agent_governor_decision` event carrying
the `payload_keys` it actually received, and `governors_selftest()` asserts the
canonical `agent_compute_post_turn` shape end-to-end. A conformance test drives a
real `agent_loop` turn through the governor, so a payload-shape drift fails CI
instead of silently disabling the mechanism.
