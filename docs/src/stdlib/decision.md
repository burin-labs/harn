# Semantic dispatch

Import `router`, `state`, and the `Decision*` types from `std/decision`.
The [source contracts](../../../crates/harn-stdlib/src/stdlib/decision/contracts.harn)
define the complete field types.

## Construction

`router<T, R>(options: DecisionRouterOptions<T, R>) -> DecisionRouter<T, R>`
requires a nonblank ID and instructions, a `SchemaContract<T>`, at least two
routes, and a policy. Each route requires a unique nonblank name, a nonblank
meaning, a minimum probability in `(0.5, 1]`, and a typed handler.

`DecisionPolicy` contains:

| Field | Contract |
| --- | --- |
| `evaluation` | Existing `EvaluationPolicy`, with `backend: "native_decision"` and `threshold: 0.0` |
| `applicability_accept` | Probability in `(0.5, 1]` |
| `applicability_reject` | Probability in `[0, 0.5)` |
| `minimum_margin` | Probability difference in `[0, 1]` |

Invalid definitions throw during construction. The evaluator validates provider
options and enforces budgets when an assessment runs. Structured LLM confidence
isn't accepted as a native probability distribution.

The returned router contains `definition`, `assess`, and `dispatch`.
`definition` projects route meanings and policy into both model questions.
Its digest covers the model-visible definition, schema projection, validation
rule names, defaults policy, and thresholds. It doesn't hash handler or
validation function bodies; use run provenance to identify executable source.

## Assessment

`assess(llm: HarnessLlm, input: unknown) -> DecisionAssessment<T>` validates and
normalizes input through its schema contract once. It evaluates the
`applicable` boolean and `route` choice together at site
`stdlib.decision.dispatch.v1`.

The applicability question tests whether any route fits. An applicability
probability at or below `applicability_reject` produces `unmatched`. A value below
`applicability_accept` produces `uncertain`. Otherwise, the selected route must
meet its minimum probability and exceed the runner-up by `minimum_margin`.
Equality passes the route and margin thresholds.

| `kind` | Payload |
| --- | --- |
| `matched` | Validated `input` and `evidence` |
| `invalid_input` | Schema contract `error` |
| `unmatched` | `evidence` |
| `uncertain` | `reason` and `evidence` |
| `evaluation_failed` | Complete existing `evaluation` outcome |

Uncertainty reasons are `applicability`, `route_probability`, and `route_margin`.
No assessment invokes a handler.

## Dispatch

`dispatch(harness: Harness, input: unknown) -> DecisionDispatch<R>` runs the same
assessment. A match invokes exactly the selected handler once and returns
`{kind: "dispatched", value: R, evidence: DecisionEvidence}`. Other outcomes
pass through unchanged. Handler errors propagate without retries or fallback.

Evidence contains definition ID and digest, receipt reference, applicability
probability, selected route probability, runner-up margin, and both confidence
kinds. Native probabilities remain uncalibrated until measured against labeled
data. Fixtures retain their own confidence provenance.

## Fixtures and replay

`state(definition: DecisionDefinition, input_json: string) -> DecisionState`
projects the exact evaluator state: definition ID and digest, serialized input,
and route meanings. Use this function with `definition.questions` and
`definition.policy.evaluation` when declaring `EvaluationFixture` requests.
Serialize the validated, normalized input rather than the original payload.

The dispatcher uses the existing [evaluation contract](../decision-evaluation-reference.md).
Receipt retention, cache, recording, replay, cancellation, and budget behavior
remain owned by that evaluator. A missing fixture never falls through to live
inference. See the [request-routing guide](../semantic-dispatch.md) for a runnable
HTTP service.

Evaluation replay still invokes the selected handler. Handler effects and agent
calls need their own fixtures or recordings. Use `assess` to replay only the
selection.
