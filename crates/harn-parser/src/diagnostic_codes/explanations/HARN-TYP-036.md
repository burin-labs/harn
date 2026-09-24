# HARN-TYP-036: Evaluation question set is not readable

`harness.llm.evaluate` reads its question set at check time. Two obligations
depend on it. The site manifest records which questions a site asks, so tooling
can identify hidden model work through a helper. The checker types each answer
from its own question, so a choice answer's `choice` is the literal union of
that question's criteria keys and a `match` on it is exhaustive.

Declare the questions as a dict literal at the call, with each value built by
`boolean`, `choice`, or `score` from `std/predicate` and each label list
written out:

```harn,ignore
const answers = harness.llm.evaluate("triage.v1", window, {
  disposition: choice("Keep, reword, or drop?", {
    keep: "Still load-bearing",
    drop: "Superseded",
  }),
  risk: score("How much blast radius?", ["none", "low", "high"]),
  safe: boolean("Safe to run without asking?"),
}, policy)
```

Question ids must be unique within a site and each question's labels unique
within that question, because answers are keyed by id and probabilities by
label. A question built elsewhere, assembled in a loop, or passed in as a
parameter cannot be read here; move the literal to the call and pass the parts
that vary as state instead.

This check makes no provider request. It establishes neither that a question is
suitable for machine judgment nor that the route supports the question kinds it
declares; runtime admission owns the route's declared limits and kinds.
