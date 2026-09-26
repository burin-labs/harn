# HARN-LNT-079 — native decision route given an unbounded input

An evaluation site hands a route the state it will encode. A structured-LLM
route has an escape when that state is too big: the ceiling measures it against
the route's window and returns `state_too_large` before dispatching anything,
so an oversized input costs nothing and the caller can react.

A native decision route does not have that escape. Its admission bounds encoded
input and question count as a condition of being usable at all, so a route
whose input carries no bound cannot establish one.

The declared type is where the bound either exists or does not. `int`, `bool`,
`float` and string-literal enums encode to a bounded number of tokens no matter
what value arrives. `string`, `list<T>`, `dict<K, V>`, `any` and an open record
admit arbitrarily many, so no window is large enough by construction, and
whether the site works depends on data its author never sees.

This rule reports an evaluation site whose policy names the `native_decision`
backend and whose input has a declared type containing one of those unbounded
constructs.

## How to fix

Narrow the declared type so its encoded size is bounded:

```harn
type Triage = {severity: "low" | "high", reopened: bool, age_days: int}

const verdict = harness.llm.evaluate(
  "triage.v1", triage, questions, policy,
)
```

Or keep the wide input and split it into windows that each fit, using the
evaluator's own estimator rather than a character approximation:

```harn
import { evaluation_windows } from "std/predicate"

const windowing = evaluation_windows(harness.llm, items, {
  anchor: latest_user_message,
  budget_tokens: 28000,
  overlap_items: 2,
})
```

Each window measures under the ceiling with the same call the ceiling makes, so
the fit is by construction rather than by retrying after a refusal.

## Severity

This reports as a warning. A bound the rule cannot see may still exist: it reads
declared types in one file, so a type that arrives through an import, or a
binding with no annotation, is not reported. Silence here means the rule found
no unbounded construct it could read, not that the input is proven bounded.
