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
harness.stdio.log(decision.recovery.verb) // stop_on_green
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
  const policy = {
    budget: 10.0, checkpoint: 1.0, over_estimate: 2.0, hard: 3.0,
    signal: "iterations",
  }
  const decision = governor_decision(
    policy,
    {
      ceiling: 10.0,
      consumed: consumed,
      made_progress: made_progress,
      signal: "iterations",
    },
  )
  return decision.action
}
```

`governor_decision` is a pure function; `governor_post_turn(policy)` wraps it
into a live `post_turn_callback` that reads the per-turn payload and steers.

```harn,ignore
import { governor_post_turn } from "std/agent/governors"

agent_loop(harness, task, ctx, {
  post_turn_callback: governor_post_turn({
    budget: 40.0, signal: "iterations",
  }),
})
```

This governor uses the shared `proceed`, `warn`, and `abort` vocabulary. Use a
[pace cut rule](./cut-rules.md) when the caller also needs to extend a wall-clock
budget.

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
token-runaway core. They compare input tokens with `median + sigma*stddev` and
use `3.0` as the hard overshoot multiple.

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
