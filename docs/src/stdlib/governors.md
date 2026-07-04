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
  let policy = {budget: 10.0, checkpoint: 1.0, over_estimate: 2.0, hard: 3.0, signal: "iterations"}
  let decision = governor_decision(
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

## Unified detectors

A `DetectorSpec` is the single typed surface for all four detectors. The loop,
no-progress, and stuck rows lower onto the native `stall_diagnostics` config;
token-runaway is added as a `post_turn_callback` overlay that emits the same
`agent_loop_stall_warning` event.

```harn,ignore
import { with_governance } from "std/agent/governors"

let opts = with_governance(base_opts, {
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
