Approval-review schema builders, decision records, screen results, and reviewer
callbacks now retain their generated field types. Partial policy and usage
references are decoded at their existing owners while preserving nominal
containers, extra fields, scalar fallbacks, and malformed-value refusals.
Runtime grants, denials, memoization, and cost accounting remain unchanged.
Refs #9347.

Migration:

Decision fields such as `approved` now reject incompatible assignments. Treat
`cost_usd`, `risk`, and `authorization` as nullable rather than assuming a
reviewer answered or a provider reported its cost:

```harn,ignore
const cost = decision.cost_usd
if cost != nil {
  harness.stdio.println(cost * 100.0)
}
```

Use `ApprovalReviewDecision`, `ApprovalReviewer`,
`ApprovalReviewScreenVerdict`, and `ApprovalReviewVerdictSchema` for generated
results. Decode opaque application metadata before accessing its fields.
