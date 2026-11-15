# Why qualify learned workflows?

A learned workflow lets an agent reuse a reviewed executable for a familiar
request. A decision model checks whether the request fits; a language model
binds arguments to the tool's schema. Harn then runs the ordinary tool path.

For example, a coding agent might learn how to add one resource to a particular
application. Later requests can reuse that program instead of generating its
implementation again. Requests outside its declared capability continue through
the agent's existing caller.

## What qualification establishes

Selector agreement alone doesn't establish that a workflow works. Qualification
uses fresh executions in isolated workspaces, independent expected labels, and
a grader that checks the resulting behavior. An accepted selection without a
successful execution counts as an error. Reused execution receipts are refused.

[`learned_qualify`](stdlib/learned-workflows.md) uses the existing selective-risk
report to choose among thresholds declared before measuring the corpus. Empty
evidence, missing negative cases, and insufficient accepted executions leave the
workflow unqualified. Statistical guarantees require the sampling assumptions
in [`std/eval/selective_risk`](eval-selective-risk-reference.md).
A synthetic corpus supports a bounded experiment; it doesn't establish
production accuracy.

## What can invalidate the result

Qualification binds the decision definition, tool descriptors and package,
project preconditions, verifier, argument binder, runtime, grader, and declared
population. A host supplies these measured inputs through the typed contracts.
Changing them requires new evidence. A source revision alone doesn't identify
the running runtime; use its [linked content fingerprint](runtime-content-fingerprint.md).

The middleware checks identity before selection and again before dispatch.
Unavailable measurements cause fallback. It also requires the original user
request to remain current, so a continuation or steer can't silently reuse an
earlier selection.

## Execution keeps its existing owner

Learning adds no tool authority. Schema validation, approvals, cancellation,
mutation policy, and tool audit remain in the normal dispatcher. A successful
local completion requires a mutation receipt with fresh verified paths.
Unverified execution returns control to the original caller with the actual
tool history, including any mutation that already happened.

Decision and binding calls retain their physical provider accounting. Selecting
a tool or producing a local completion doesn't fabricate a provider call.
The transcript records selection, execution mode, evaluation receipts, fallback,
and verification so replay and evaluation can inspect the path that ran.
