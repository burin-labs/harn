# Evidence-aware repair loop (repair-diagnostics)

The agent loop's stall detector
(`crates/harn-stdlib/src/stdlib/agent/stall.harn`) historically tracked a blind
counter of repeated identical actions. That counter cannot tell the difference
between a model that is *thrashing* (re-running the same failing test with no
change) and a model that is *making legitimate progress* (edit, re-test, edit,
re-test) — both look like "the same action again". It also let a model declare
victory right after a corrective edit without ever re-running the check that was
failing.

The evidence-aware repair loop replaces that blind counter, for repair-shaped
work, with a **current-failure model**: which diagnostic is currently failing,
how many corrective edits have been attempted against it, and whether the last
edit has actually been re-verified.

It is **default OFF** (`repair_aware: false`). When off, every repair field is
inert and the detector behaves exactly as before — verified by
`conformance/tests/agents/repair_diagnostics_off_is_noop.harn` and the unchanged
218-test agents conformance suite. Burin opts in via a feature flag.

## The four behaviors

1. **Current-failure model, not a blind counter.** `AgentStallState` carries
   `last_diagnostic_class` (ProbeOutcome-shaped `pass`/`fail`), a
   `last_diagnostic_signature` (the SHA-256 of the normalized failure evidence,
   reusing `__agent_stall_result_outcome` — never re-implemented), a
   `write_epoch` bumped on each successful workspace-mutating tool (it arms the
   post-edit re-verify mandate), a `same_diagnostic_streak`,
   `edit_since_failure`, and `reverify_owed`. These six fields are folded each
   turn by `__agent_stall_fold_diagnostic`.

2. **Post-edit re-verify mandate.** A successful corrective edit is *not* proof
   the failure is resolved. When `post_edit_reverify` is set and a turn makes a
   successful edit on top of a live failure, `agent_loop` forces a verification
   through the **existing** `verify_completion` / `verify_completion_judge`
   entrypoint (`agent_verify_or_continue`) — even when the model proposed
   `continue`, and before allowing it to stop `done`. It routes through the
   existing `max_verify_attempts` cap; there is no new verify engine.

3. **Strategy-shift nudge on a stuck diagnostic.** When the *same* failure
   **signature** recurs across `stuck_same_diagnostic_after` repair turns — even
   when corrective edits intervened, as long as those edits did not change the
   error — the detector trips a new `stuck_same_diagnostic` pattern on the
   **existing** `agent_loop_stall_warning` event, riding the existing
   `__agent_stall_register_trip` escalation. The trip fires only on the turn the
   streak *advances* to the threshold (a fresh same-signature failure), so an
   edit/owe turn that holds the streak at the threshold does not re-trip. The
   injected feedback is grounded in the actual diagnostic snippet ("the same
   failure has persisted... the failing evidence is unchanged: `<the snippet>`"),
   nudging the model to change approach rather than retry harder.

4. **Failure-carrying hand-back.** A stuck terminal stop carries a
   `current_failure` block (`{class, signature, snippet,
   same_diagnostic_streak}`) on the run result via `agent_stall_apply_result`,
   so the hand-back says *what* is failing, not just "budget exhausted" /
   "thrash". A **successful** termination (clean `done` / a passing
   `verify_completion`) clears the model first, so it never reports a stale
   failure (`agent_stall_clear_current_failure`).

## The signature IS the progress signal

The key correctness property is in the streak fold, and it is **signature-keyed,
not epoch-keyed**. The failure signature — the SHA-256 of the normalized failure
evidence — is the progress signal:

- **same failure signature** ⇒ futile retry (`streak + 1`), **even across
  intervening edits**. An edit that leaves the *same* error did not make
  progress; that is exactly the edit-between-retest thrash the feature exists to
  catch. (An earlier design keyed the streak on a write-epoch and reset it on
  *every* successful edit — which meant the `fail, edit, fail, edit, fail`
  thrash case could never trip. That was the bug this design fixes.)
- **different signature** ⇒ reset to `1`. A productive edit *changes* the error,
  so a different (or passing) signature is real progress and is never flagged.
  This is the false-positive safety property: progress always resets the streak.
- **pass** ⇒ clear the streak, `reverify_owed`, and `edit_since_failure`.

Because the streak can sit at the threshold across a no-result edit turn (which
*preserves* the model and owes a re-verify rather than re-folding a failure), the
trip only fires on the turn the streak **advances** to the threshold, so it
nudges once per stuck episode rather than on every subsequent turn.

A turn that BOTH edits and re-tests still classifies: `__agent_stall_turn_result`
returns the first *failing* dispatch result, so the re-test's failure is seen
past the edit's own "ok". A non-failing result on an edit turn is the edit's own
success, *not* verification of the failure, so it owes a re-verify and preserves
the model rather than clearing it.

Proven by:

- `conformance/tests/agents/repair_edit_retest_thrash_trips.harn` and
  `tests/agent/stall_test.harn::test_fold_edit_retest_thrash_increments_streak`
  (the primary case: same signature across edits ⇒ trips).
- `conformance/tests/agents/repair_edit_and_test_same_turn_thrash.harn`
  (single-turn edit+retest with the same signature ⇒ trips).
- `conformance/tests/agents/repair_post_edit_resets_streak.harn` and
  `tests/agent/stall_test.harn::test_fold_productive_edit_resets_streak`
  (a *different* failure after an edit ⇒ resets, no trip).
- `tests/agent/stall_test.harn::test_clear_current_failure_on_success` and
  `conformance/tests/agents/repair_success_clears_current_failure.harn` (no
  stale `current_failure` on a successful hand-back; still carried when stuck).

The current-failure fields are **deliberately not cleared by
`__agent_stall_reset_action`** — a *different* corrective action does not clear
"same root failure". This survives a tool-stream reset, verified by
`tests/agent/stall_test.harn::test_reset_action_preserves_failure_model`.

## One-turn fold lag

The detector observes a turn's tool calls at the *start* of the turn, but the
verification result it must classify comes from the *prior* turn's dispatch
(`prev_dispatch`). The current-failure model is therefore folded from the prior
turn, so the streak lags the action stream by one turn. With
`stuck_same_diagnostic_after: N`, the strategy-shift trip fires after the
`N+1`-th identical failing turn (folding the `N`-th same-failure observation).
This is inherent to the existing observe seam and is documented in the
conformance tests. The post-edit re-verify mandate side-steps the lag by
computing the current turn's edit directly from its dispatch at the post-turn
branch.

## Interaction with the legacy repeated-failure patterns

When `repair_aware` is enabled it **owns** the repeated-failure semantics. The
legacy `repeated_error` and `repeated_same_observation` trips are suppressed
(`__agent_stall_classify_trip`) so the two do not double-fire and the richer,
diagnostic-grounded `stuck_same_diagnostic` nudge wins. The orthogonal
context-window and ping-pong patterns are untouched, as is the provider-only
`consecutive_failure_count` transport breaker in `loop.harn`.

## Surface

The knobs ride inside `stall_diagnostics` (where `__agent_stall_config` parses
them) and are also mirrored on the `repair_diagnostics` discovery block of the
tool-using defaults preset (`presets.harn`), default-safe-off:

- `repair_aware` (bool, default `false`)
- `stuck_same_diagnostic_after` (int, default `3`)
- `post_edit_reverify` (bool, default `true`)
- `no_net_progress_extend_guard` (bool, default `false`)
- `no_net_progress_extend_after` (int, default `3`)
- `no_net_progress_hard_cap_after` (int, default `16`)

When `no_net_progress_extend_guard` is enabled, Harn now uses two complementary
signals. The same-diagnostic threshold catches flat stalls early and suppresses
adaptive budget extension. The hard cap counts every failing verification turn
and resets only on a clean non-edit verification pass, so a model cannot evade
recovery by slowly changing error signatures or interleaving successful edits
with still-red verifies. Crossing the hard cap emits
`no_net_progress_hard_cap` and terminates through the existing `stuck` path.

## Subsumption of Burin's futile-retry-guard.harn

This feature subsumes Burin's product-side `futile-retry-guard.harn`. That guard
was a Burin-local approximation of the same intent — detect a model retrying a
fix that is not working and steer it off — but it lived on the host product
layer, and could not force a re-verify through the loop's own verify entrypoint.
The signature-keyed streak here catches the exact case the guard targets: the
`fail, edit, fail, edit, fail` edit-between-retest thrash trips
`stuck_same_diagnostic` (proven by `repair_edit_retest_thrash_trips.harn`),
while a productive edit that changes the error never trips, so the subsumption
is real and not merely a single-failure counter.

Because retry/verify/stuck policy is Harn-owned (orchestration policy,
transcript lifecycle, retry/verify/repair all belong in Harn per the Burin/Harn
ownership boundary), the durable home for this behavior is here in
`harn-stdlib`. Migration plan for Burin:

1. Land this feature in Harn (default-off), repin Burin to the new Harn version.
2. Flip the Burin feature flag that opts the tool-using preset into
   `repair_aware: true` for the agent loop (the flag gates the opt-in, not the
   default).
3. Delete `futile-retry-guard.harn` and its wiring from Burin once the
   flag-gated meter run confirms parity-or-better, so there is a single
   evidence-aware repair authority rather than two overlapping heuristics.
