The public `hypothesis_ledger_snapshot` now verifies, decodes, and projects one
retained history in a single pass while returning the retained-topic integrity
scope and last hash. Typed completion evidence distinguishes statistical
stopping from exact max-trial, natively attested budget, and wall-clock
exhaustion, so early decisions no longer inherit `budget_spent: true`. A closed
`hypothesis_workflow` state machine inspects start, resume, inspect, and
stand-down requests and returns `adapter_unavailable` without recording fake
lifecycle events when no native operation adapter exists.
