# Host-supplied facts

Some of the most useful signals for steering an agent live outside the loop. The
loop can count iterations and tokens and spot a repeated error, but it cannot
know whether your test runner just went green, whether the fix it delivered
actually landed, or how much longer this particular job deserves. Only your host
knows those things.

Harn's detectors and governors own the *mechanism*: when to warn, when to cut a
run that's spinning in place. Your host owns the *facts* that mechanism runs on.
You supply those facts through three callbacks. Each one gets the loop's
per-turn payload and returns one small answer; Harn does the rest.

This is the pattern the Burin coding agent runs on: Harn decides *whether*
progress stalled, Burin answers *did the build get closer to green*.

| Callback | Lives on | You answer | Harn decides |
|---|---|---|---|
| `progress_signal` | `stall_diagnostics` | "did verification advance this turn?" | whether the run stopped making net progress |
| `remediation_delivered` | `stall_diagnostics` | "did the fix we just sent actually get applied?" | whether a delivered-but-ineffective fix is a stall |
| `governor_pace_decision` | pure function you call | (the observation of budget + progress) | proceed / extend / pace-check / cut |

## `progress_signal`: writes are not progress

The problem this fixes: an agent that edits a file every single turn *looks*
busy, but if the test count never moves, it's stuck. Time and edits are not
progress. A verifier advancing is.

Set `progress_signal` in `stall_diagnostics` to a closure that returns the
turn's best verify-state as a number: the count of passing tests, or `1` when
the build is clean and `0` when it isn't. Return `nil` when this turn produced
no verification evidence at all.

```harn,ignore
let opts = {...base, stall_diagnostics: {
  progress_signal: { payload -> host_passing_test_count() },
}}
```

**Signature.** `progress_signal(payload) -> int | float | nil`. The `payload`
is the full post-turn payload the loop already assembles; most hosts ignore it
and read their own runner state. The callback must be a closure or `nil`.

**How Harn reads the return.** Harn keeps a high-water mark of the best
verify-state seen so far:

- A number **above** the high-water mark (or the first number ever) is an
  *advance*: the mark rises and the no-progress streak resets to zero.
- A number **at or below** the high-water mark is *flat*: the streak grows.
- `nil` is *flat*: the streak grows, and if a write landed this turn, that's
  recorded too.

**The cut.** With `progress_signal` set, `agent_stall_no_net_progress` stops the
run on either of two conditions:

- **Long stall.** The same diagnostic has recurred for at least
  `verify_state_streak` turns (default 3) *and* verify-state has not advanced for
  at least `verify_state_stall_turns` turns (default 12). Or the same diagnostic
  has hard-recurred `verify_state_recurrence_hard` times (default 4).
- **Write-axis short cut.** Verify-state has not advanced for at least
  `no_verifier_progress_limit` turns (default 3) *and* a write landed during that
  non-advancing streak. This is the "edit lands every turn but the verifier never
  moves" case, and it cuts without waiting out the long stall.

The two axes are deliberate. Long-stall catches slow spinning; the write-axis
cut catches fast, confident, wrong.

## `remediation_delivered`: the fix didn't land

The problem this fixes: an agent delivers a fix, the fix has no effect, and the
same failure comes back. That's a stronger stall signal than a failure the agent
never touched. The agent tried, and its try didn't work. It should escalate
sooner.

Set `remediation_delivered` in `stall_diagnostics` to a closure that returns
whether the last fix Harn asked for was actually applied on your side.

```harn,ignore
let opts = {...base, stall_diagnostics: {
  remediation_delivered: { info -> host_last_patch_applied(info.session_id) },
}}
```

**Signature.** `remediation_delivered({prev_dispatch, session_id, signature}) ->
bool`. Harn calls it with the previous tool dispatch, the session id, and the
current diagnostic signature. Return `true` when the fix reached the workspace.
The callback must be a closure or `nil`.

**When it fires.** This is the highest-precedence stall signal. Harn considers it
only when the current diagnostic is a failure, the same diagnostic has repeated
at least `STALL_DELIVERED_FIX_AFTER` times (default 2, one turn sooner than the
plain repeated-failure nudge at 3), the streak just advanced this turn, and no
warning has already fired. If your callback then returns `true`, Harn emits a
stall warning with pattern `delivered_fix_not_landing`.

Harn owns only the trigger. What to do about a delivered-but-ineffective fix
(switch strategy, escalate to a human, widen the tool surface) is your host's
call. It fires whether or not the loop is otherwise repair-aware.

## `governor_pace_decision`: a smart timeout

The problem this fixes: a fixed wall-clock timeout is dumb. It cuts a run that's
one turn from done and lets a run that stalled at the checkpoint keep burning. A
pace decision looks at progress and budget together and decides.

`governor_pace_decision` is a pure function. You feed it a policy and an
observation of the current run, and it returns an action. Nothing about it
touches the loop directly. You call it and act on the verdict.

```harn,ignore
import { governor_pace_decision } from "std/agent/governors"

let decision = governor_pace_decision(
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
// decision.action -> "proceed" | "extend" | "pace_check" | "cut"
```

**Signature.** `governor_pace_decision(policy: GovernorPolicy, obs: dict) ->
dict`.

**Policy.** Two bounded caps: `extend_max` (default 6) limits how many silent
extensions a run can earn, and `pace_check_max` (default 2) limits how many
pace-check nudges fire before the run is cut.

**Observation.** `armed_budget_ms`, `elapsed_ms`, `checkpoint_ms`,
`expected_total_ms`, `made_progress`, `verifier_signature_unchanged`, `done`,
`extends_used`, `pace_checks_used`, `env_blame_without_infra`.

**The verdict.** One of:

- `{action: "proceed"}` — keep going. Returned when the budget is inert
  (`armed_budget_ms <= 0`), the run is `done`, or the checkpoint hasn't been
  reached yet.
- `{action: "extend", new_budget_ms, reason}` — the run is progressing and still
  within its estimate, so extend it silently. `new_budget_ms` is `elapsed_ms +
  checkpoint_ms`, and `new_budget_ms` is present *only* on this action. Bounded
  by `extend_max`; past that it becomes a cut with reason
  `extend_budget_exhausted`.
- `{action: "pace_check", reason}` — the run is past its estimate but still
  moving; nudge it to wrap up. Fires up to `pace_check_max` times, then cuts.
- `{action: "cut", reason}` — stop. Reasons: `no_progress_at_checkpoint` (no
  progress by the checkpoint), `no_verifier_progress` (writes happened but the
  verifier signature is unchanged; same rule as `progress_signal`, where
  writes are not progress), `env_blame_without_infra`,
  `extend_budget_exhausted`, `pace_check_budget_exhausted`.

A silent extend is the point of the whole thing: a run that's genuinely making
progress buys itself more time without a human ever seeing a timeout, and a run
that's stalled gets cut at the checkpoint instead of at the wall.

## See also

- [Agent governors and detectors](./governors.md) — the mechanism side: the
  budget governor and the unified detector surface these facts feed.
- [Steering seams](../concepts/steering-seams.md) — the broader picture of
  out-of-band influence on a running loop, and where host-supplied facts fit.
- [Typed facts](../memory.md#typed-facts) — durable, queryable claims an agent
  stores, distinct from these per-turn signals.
- [The expressiveness spectrum](../concepts/expressiveness-spectrum.md) — where
  host-supplied facts sit on the ladder from a three-line call to full control.
