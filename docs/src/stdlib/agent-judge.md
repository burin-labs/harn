# Completion judge

The loop decides whether a turn may end through three seams:
`verify_completion` (a host callback that reads deterministic facts such as
which writes changed source and whether verification passed),
`verify_completion_judge` (a bounded LLM judge that runs after it), and
`turn_end_condition`. Harn owns the judge and its arbitration. The host owns the
facts and supplies them from its own callback.

## The bounded judge

Set `verify_completion_judge` to a judge config to add an LLM check after the
`verify_completion` callback. An empty dict uses defaults. A dict may set the
provider, model, system prompt, timeouts, and invocation cap. Set
`turn_end_condition` to use that completion trigger instead. The default
cap is 5 calls per session. Past the cap, the loop ends with status
`completion_unverified`. Set `max_invocations: 0` to disable the cap. Two
helpers expose the catalog review and the resolved cap:

```text
agent_completion_review(llm, opts) -> dict
agent_verify_completion_judge_cap(judge_cfg, review?) -> int | nil
agent_turn_end_judge_cap(judge_cfg) -> int | nil
```

`agent_turn_end_judge_cap` is exported by `std/agent/turn_end`, which owns the
turn-end condition end to end: whether one is configured, whether its bounded
judge is due at this boundary, and how its caps and invocation counts are
resolved. This module holds the product question "may this turn end?" and
nothing else; `make check-turn-end-boundary` fails if grading vocabulary
appears in it, or if an eval or bench module reaches into its internals instead
of reading the public `result.turn_end_condition` block off a finished session.

`agent_completion_review` reads the session model's catalog row and returns
`{scrutiny: "standard"}` when the row omits `completion_review`. The
verification-judge cap prefers an explicit `max_invocations`, then catalog
`max_judge_calls`, then 5. Both cap helpers return `nil` when the cap is
disabled.

### What the judge is shown, and what it may refuse on

The `verify_completion` verifier reading is passed to the judge rather than discarded. The
evidence snapshot carries a typed
`verification: {oracle_expected, command, observed, observed_at_evidence_index}`,
where `observed` is `passed`, `failed`, or `not_run`, and the judge's prompt
renders it as an explicit deterministic-verification block. `not_run` covers
both "no oracle was configured" and "the callback could not read facts", so a
`facts_unavailable` allow can never read as a passing verification.

The judge's verdict carries a `gap_class` alongside `verdict` and `detail`:
`missing_artifact`, `unmet_manner_clause`, `failed_verification`,
`unresolved_authorization`, or `other`. It is optional on the wire — an absent
or unrecognized value reads as `other`.

A `continue` naming `failed_verification` is converted to `done` when, and only
when, all four hold: the deterministic stage actually ran (the directive
receipt's `invoked.deterministic`), the threaded `observed` is `passed`, the
gate's reason is not `facts_unavailable`, and the named class is
`failed_verification`. The converted directive carries
`converted_from: "failed_verification_contradicted_by_gate"`. Count conversions
by `converted_from`; a receipt's `trigger` names whichever adjudicator answered,
not the boundary that asked.

Every other `gap_class` vetoes exactly as before. The judge keeps sole authority
over artifact clauses, manner and negative clauses, and authorization, and loses
it only over the one question a deterministic oracle has already answered.

### Turn phase events

Agent streams emit `turn_phase_changed` with a session id and a typed `phase`.
ACP carries the same payload through `_harn/agentEvent`. Events arrive in turn
order for each session.

| Phase | Payload | Meaning |
| --- | --- | --- |
| `generating` | none | The actor is generating, including a re-ask or final wrap-up. |
| `verifying` | `candidate_reply` | Completion review is running. The candidate reply is provisional. |
| `terminal` | `reply`, `outcome` | Session finalization supplies the final visible reply and typed stop outcome. |

Verification starts before deterministic checks, prechecks, model judges, and
arbitration. `judge_started` still identifies a model judge call within that
phase. A rejected candidate can lead to another `generating` phase.

Hosts should hold or de-emphasize assistant text until `terminal` supplies
`reply`. Neither `iteration_end` nor a `done` judge verdict makes text terminal.
`outcome.kind` distinguishes natural completion from cancellation, failure,
policy stops, and suspension. A withdrawn candidate can leave `reply` empty.
Admission denials and initialization failures emit `terminal` with an empty
reply before any provider call.
The phase events survive canonical session replay. Older recordings without
these events don't establish reply finality through this contract.

Rust consumers must handle `AgentEvent::TurnPhaseChanged` in exhaustive event
matches. Its `AgentTurnPhase` value carries the phase-specific reply fields.

### When the judge is not called at all

Neither judge seam is called when the runtime already holds the answer. All seven
of these must hold:

| Clause | Required reading |
| --- | --- |
| the callback ran | the directive receipt's `invoked.deterministic` |
| the callback allowed on a proven write | reason is exactly `verified_after_write` |
| the reading agrees with the reason | threaded `observed` is `passed` |
| the turn is the sealed final answer | `stop_reason` is `sentinel` |
| there is an answer to hand back | the final response is non-empty |
| nothing is deferred past the turn | `pending_tool_batch_effect_count` is zero |
| no acceptance row is outstanding | the ledger's `pending_count` is zero |

No provider call is made and no `judge_started` event is emitted. The directive
seals as `accept` with `source: "gate"` and
`outcome: "skipped_verified_after_write"`, and `invoked.turn_end_condition` stays
false, so a reader can tell a judge that never ran from one that ran and agreed.

`verified` is deliberately not `verified_after_write`. A green verifier with no
source write behind it is an ordinary prose completion and still calls the
judge, as do an unverified path, a red or unrun verifier, a non-sentinel
boundary, and a run with no deterministic callback.

The acceptance-ledger clause is there because the skip does not only skip the
judge. The pending-requirements check that turns a `done` into a `continue` lives
inside the structured invocation, so a judge that is never asked takes the ledger
with it. A declared row can only be established as met by an assessment, so with
no assessment in hand every declared row is pending and the judge is asked. What
the clause never does is let the skip fire *because* a ledger came back met.

The judge cap reads the same ledger. A cap reached over a passing verifier
converts to `stop_verified`, which seals `done`; that conversion now also
requires the ledger to be clear, because a cap is a budget rather than an
assessment and the judge may have spent its whole budget refusing the row. With
no rows outstanding the conversion is unchanged and still reports
`judge_cap_reached_over_verified_pass`.

A `done` sealed without a judge now records an explicit empty ledger rather than
`nil`. `nil` reads the same for "no rows were declared" and "rows were declared
and nobody looked", and only the second is a false completion.

One predicate answers for both slots, read at the single point where the plan
decides whether to announce a judge. The catalog-declared `completion_review`
light-scrutiny skip is subordinate to it: that rule never reads the verification
at all, so on a typed green terminal it was deciding, from a model's catalog row,
a question the oracle had already answered. It now decides only what the
verification did not, and its receipt still carries `source: "catalog"` so a
reader can tell the two skips apart. The ledger clause applies to that skip too,
for the same reason: a skipped judge is a skipped pending-requirements check,
whichever rule did the skipping.

Projection receipts name their selected actions and resolved evidence roles in
`selected_actions`, so a host whose verifier declares no
`completion_evidence_role` — and whose passing verification is therefore counted
and then dropped from the bounded packet — is visible without arithmetic on the
counts.

### Optional negative completion precheck

`JudgeConfig.precheck` accepts a `CompletionPrecheckConfig` from
`std/agent/completion_precheck`. It runs after existing deterministic decisions,
judge caps, and catalog skips, before announcing or invoking the full judge.
It is absent by default and cannot authorize completion.

The config contains `policy` and an optional `facts(session_id, evidence_id)`
callback. The callback supplies observed commit and pull-request facts, each
with `observed: bool?` and `evidence_refs: list<string>`. An absent callback
means unknown facts. The runtime projects the requirement ledger, verifier
reading, and last assistant claim into the remaining input. Accepted stops or
amended goals bypass this precheck so the full judge owns their interpretation.

The policy contains an `EvaluationPolicy`, an `achieved_ceiling` in `[0, 0.5)`,
and a `missing_confidence_floor` in `(0.5, 1]`. One evaluation asks whether the
requirements are achieved and which named requirement is missing. Only a
negative answer meeting both authored thresholds returns `continue` with that
gap. These thresholds are configuration, not a calibrated accuracy guarantee.
When using a run-cost ceiling, install the conservative parent budget before
the first actor call; a late precheck cannot retroactively establish accounting
for earlier calls. The per-evaluation ceiling remains a separate bound.

Positive, uncertain, malformed, and unavailable answers fall through to the
full judge. Inputs exceeding 12,000 UTF-8 bytes are refused without truncation.
Cancellation and parent-budget, deadline, or run-cost refusal return
`control_stop`, which ends the checkpoint unverified. Existing deterministic
verification and requirement rules retain their authority.

The directive receipt records `precheck`, including its input digest, decision,
and available evaluation receipt. A precheck veto leaves the full-judge
invocation false and emits no `judge_started` event; the normal loop delivers
the named gap to the next actor turn. Standalone callers can use
`completion_precheck(llm, input, policy)` with the same typed input and policy.

## See also

- [Completion control](../llm/completion-control.md)
  — the judge in the context of the loop's other completion seams
  (`verify_completion`, `verify_completion_judge`, `turn_end_condition`).
- [Host-supplied facts](./fact-intake-seams.md) — the broader
  Harn-owns-mechanism / host-owns-facts pattern the judge follows.
- [Agent detectors](./detectors.md) — the stall side of
  the same placement contract.
- [The expressiveness spectrum](../concepts/expressiveness-spectrum.md#level-4-tune-the-loop-by-composing-building-blocks)
  — where completion control sits when you compose loop control by hand.
