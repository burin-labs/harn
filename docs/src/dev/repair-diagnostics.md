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
   `write_epoch` bumped on each successful workspace-mutating tool, the
   `diagnostic_epoch_at_failure`, a `same_diagnostic_streak`,
   `edit_since_failure`, and `reverify_owed`. These seven fields are folded each
   turn by `__agent_stall_fold_diagnostic`.

2. **Post-edit re-verify mandate.** A successful corrective edit is *not* proof
   the failure is resolved. When `post_edit_reverify` is set and a turn makes a
   successful edit on top of a live failure, `agent_loop` forces a verification
   through the **existing** `verify_completion` / `verify_completion_judge`
   entrypoint (`agent_verify_or_continue`) — even when the model proposed
   `continue`, and before allowing it to stop `done`. It routes through the
   existing `max_verify_attempts` cap; there is no new verify engine.

3. **Strategy-shift nudge on a stuck diagnostic.** When the *same* diagnostic
   (same signature, same epoch — i.e. no intervening edit) recurs across
   `stuck_same_diagnostic_after` repair turns, the detector trips a new
   `stuck_same_diagnostic` pattern on the **existing** `agent_loop_stall_warning`
   event, riding the existing `__agent_stall_register_trip` escalation. The
   injected feedback is grounded in the actual diagnostic snippet ("the same
   failure has persisted... the failing evidence is unchanged: `<the snippet>`"),
   nudging the model to change approach rather than retry harder.

4. **Failure-carrying hand-back.** A stuck terminal stop carries a
   `current_failure` block (`{class, signature, snippet,
   same_diagnostic_streak}`) on the run result via `agent_stall_apply_result`,
   so the hand-back says *what* is failing, not just "budget exhausted" /
   "thrash".

## The false-positive safety property

The key correctness property is in the streak fold:

- **same signature AND same epoch-since-fail** ⇒ futile retry (`streak + 1`).
- **same signature but a NEW epoch** (a successful edit intervened) ⇒ reset to
  `1`. This is the legitimate post-edit retry: re-running the test after a real
  fix is *not* thrashing, so it must not trip.
- **different signature** ⇒ reset to `1`.
- **pass** ⇒ clear the streak, `reverify_owed`, and `edit_since_failure`.

Proven by `conformance/tests/agents/repair_post_edit_resets_streak.harn` (a
fail/edit/fail/edit/fail sequence trips a blind counter but not the epoch-aware
model) and the field-by-field unit test
`tests/agent/stall_test.harn::test_fold_post_edit_resets_streak`.

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

## Subsumption of Burin's futile-retry-guard.harn

This feature subsumes Burin's product-side `futile-retry-guard.harn`. That guard
was a Burin-local approximation of the same intent — detect a model retrying a
fix that is not working and steer it off — but it lived on the host product
layer, lacked the epoch-aware false-positive protection (it could not tell a
legitimate post-edit retry from a futile one), and could not force a re-verify
through the loop's own verify entrypoint.

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
