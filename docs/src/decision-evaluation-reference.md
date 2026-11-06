# Decision evaluation contract

`harness.llm.evaluate(id, state, questions, policy)` evaluates a set of
questions over one shared state and returns a closed `EvaluationOutcome` union.
`harness.llm.evaluate_predicate(id, question, input, policy)` is the
single-boolean projection with a `PredicateOutcome` union. The authoritative
type declarations and builders are in `std/predicate`.

## Questions and policy

The question map is keyed by stable IDs. All questions need nonempty
`instructions`.

| Builder | Question fields | Answer fields |
| --- | --- | --- |
| `boolean(instructions)` | `kind: "boolean"` | `verdict`, yes `probability`, `confidence`, evidence |
| `choice(instructions, criteria)` | `kind: "choice"`; label-to-description map | selected `choice`, per-label `probabilities`, `confidence`, evidence |
| `score(instructions, levels)` | `kind: "score"`; ordered labels, lowest first | selected `level`, fractional `score`, per-level `probabilities`, `confidence`, evidence |

Each answer carries `confidence_kind` and `evidence_kind`. The policy declares
`backend` (`structured_llm` or `native_decision`), `provider`, `model`, and a
`threshold` in `[0, 1]`. Optional `effort`, `temperature`,
`evaluation_cost_limit`, and `run_cost_limit` travel with the request. Route
limits are checked before dispatch; an invalid question returns
`question_invalid` with the offending question ID and reason, with no provider
charge.

## Outcome arms

Every arm carries a `receipt` handle. Match on `kind` before reading its
arm-specific fields.

| `kind` | Fields beyond `receipt` | Meaning |
| --- | --- | --- |
| `answered` | `value` | Every declared question has an answer at or above the threshold. |
| `low_confidence` | `candidates`, `threshold`, `question_ids` | Answers exist, but the named questions fell below the threshold. |
| `refused` | `reason`, `diagnostic` | Provider refusal, invalid response schema, or truncated output. |
| `budget_cut` | `limit`, `requested`, `remaining` | A request, token, deadline, cost, or parent budget stopped dispatch. |
| `unavailable` | `reason` | Route, authority, spend admission, transport, producer, or cache failure. |
| `replay_mismatch` | `expected_identity`, `actual_identity`, `occurrence` | The request differs from the offline record. |
| `cancelled` | `control_event` | A stop or cancellation was accepted. |
| `state_too_large` | `limit_tokens`, `estimated_tokens` | The supplied state exceeds the route's declared window. |
| `question_invalid` | `question`, `reason` | The question set violates a declared shape or route limit. |
| `rate_limited` | optional `retry_after_ms` | The provider returned a rate limit. |
| `overloaded` | none | The provider is overloaded. |

The single-boolean projection uses `verdict` with `value`, or
`low_confidence` with `candidate`, instead of the batched `answered` arm. It
shares the remaining refusal and control arms.

## Retained receipt accounting

`harness.llm.evaluation_receipt(reference)` reads the accounting projection for
an occurrence returned by any evaluator method. It makes no provider request.
`kind: "retained"` carries a typed `receipt` with schema
`harn.evaluation.accounting.v1`: occurrence and route identity, outcome,
physical attempts, accounting status, tokens, settled cost, and budget charge.
Its optional `usage_certainty` preserves the usage owner's known cost and
unpriced-call coverage when a total bill remains unknown.
Its optional `native_transport` preserves reported downstream attempts and
provider, plus the full data-control receipt, including caveats. Missing reports
remain `nil`; a budget charge doesn't establish a settled provider bill.

`kind: "unavailable"` carries `reason: "malformed_reference"`,
`"foreign_execution"`, or `"not_retained"`. Unknown occurrences and journal
overflow return `not_retained`. The execution retains at most 1024 receipts;
child VMs share that journal. Lookup doesn't create receipts or recover dropped
ones. Full receipts remain in the run record's `evidence.evaluation_receipts`.

The [replay how-to](./decision-replay.md) explains exact request binding and
the receipt's `source` and `reused_from` fields. A receipt proves what route
answered and what was charged for this invocation. It does not establish that
the answer is correct or that its confidence is calibrated; see the
[confidence explanation](./concepts/confidence.md) and
[`std/eval/calibration`](./eval-calibration-reference.md).
