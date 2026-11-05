# Agent detectors

The unified detector surface in `std/agent/stall` provides loop, no-progress,
stuck-tool, and token-runaway detection for `agent_loop`. A run's spend is
bounded by the loop's own ceilings (`max_iterations`, `token_budget`, the
`iteration_budget` rows); when one is reached the run ends with status
`budget_exhausted`.

Detectors speak the three actions the live post-turn seam can take:

| Action    | Meaning                                   | Verdict on the seam            |
|-----------|-------------------------------------------|--------------------------------|
| `proceed` | do nothing, keep the run going            | `nil`                          |
| `warn`    | inject a wrap-up reminder and continue    | `{message}`                    |
| `abort`   | stop the run before overrun               | `{stop: true, stop_reason}`    |

Detectors are **values you compose into the existing `post_turn_callback`
seam** — never a new hook on `agent_loop`. `compose_post_turn([...])` from
`std/agent/postturn` chains several callbacks into one (the first stop wins,
warnings accumulate).

## Unified detectors

A `DetectorSpec` is the single typed surface for all four detectors. The loop,
no-progress, and stuck rows lower onto the native `stall_diagnostics` config;
token-runaway is added as a `post_turn_callback` overlay that emits the same
`agent_loop_stall_warning` event.

`stall_diagnostics.observation_recurrence_advisory` defaults to `true`. Set it
to `false` to suppress the advisory for identical read results recurring across
interleaved calls. Consecutive-repeat safeguards remain enabled, and repeated
observations still count as no information gain for the iteration budget.

`stall_diagnostics.edit_oscillation` defaults to `false`. With both this option
and stall diagnostics enabled, completed workspace-edit tools report
`{path, after_sha256, mutation_status: "applied"}` in their typed result or
handler `data`. Only successful executed edits with an explicit applied
mutation count. Missing or empty hashes remain unavailable evidence.
Harn consumes the reported hash without reading files or retaining contents.
The pre-edit `before_sha256` is never recorded, so the first return to the
pristine original does not warn. A return to an agent-produced intermediate
does, even across unrelated edits or transcript compaction.

Each revisit emits one `agent_loop_stall_warning` with `kind` and `pattern`
`edit_oscillation`, `path`, `revisit_count` (one on the first revisit), and
`iteration_distance` since the latest visit to that state in the same loop.
On a checkpoint handoff to another loop/stage, that distance is `null` because
the new loop restarts its iteration counter; content identity and revisit
counts still survive. A subsequent visit within the new loop has a measured
distance again. The existing
`count`/`repeat_count` include the original visit. This evidence maps to `warn`
through `detector_action_of`; it never advances the abort ladder. A legitimate
revert or exploration can revisit content, so this is an advisory about content
recurrence, not proof of failure or lack of progress. Hosts retain the existing
proceed/warn/abort decision vocabulary. The detector emits structured evidence
without injecting a strategy-change prompt.

Visits live in `AgentStallState`, keyed by exact session, reported path, and
content hash. The checkpoint retains the 256 most recently visited distinct
states across all paths, as opaque digests and counters; revisits refresh that
order. Evicted states cannot trigger a warning until recorded again. A fresh
stall state starts a new lifetime; compaction does not. Report canonical paths
consistently. No new state is collected while disabled, and existing warning
payloads are unchanged.

`stall_diagnostics.fresh_failure_grace` defaults to `false`. When enabled, a
previously unseen verification failure defers the failing-verification cap for
that turn and the following turn. Revisiting an older failure does not renew
grace. A deferred cap remains due until it fires or verification passes; this
setting does not defer other stop conditions.

`stall_diagnostics.shrinking_failure_set_resets_budget` defaults to `false`.
When enabled, a strictly smaller positive count of distinct verification errors
resets the failing-verification budget. The producer must attest
`diagnostics_complete: true` alongside structured diagnostics and a completed
command outcome. `harness.tools.run_build_command` supplies this evidence for
fully captured terminal output. Timeouts, partial output, warnings, and empty
failed diagnostic sets cannot earn credit or replace the last completed failure
baseline. Passing verification clears that baseline; later unchanged failures
can exhaust the restarted budget.

The optional `progress_signal` callback reports a scalar whose high-water mark
is monotone within a repair episode. The first successful workspace mutation
after clean verification or successful terminal completion starts a new episode.
Edits during an unresolved failure preserve the existing high-water mark and
flat-progress counter, so repeated edits cannot manufacture progress.

`token_runaway_decision` and `token_runaway_resolve_cap` are the pure
token-runaway core. They compare input tokens with `median + sigma*stddev` and
use `3.0` as the hard overshoot multiple.
