Agent loop results and terminal transcript records now carry the same typed goal outcome,
distinguishing assessed requirement success, partial success, failure and unknown from the run's
lifecycle state. Unassessed and empty requirement contracts remain unknown without an additional judge call.

Migration: Rust callers of `AgentTerminalOutcome::checkpoint` must supply the finalized
goal outcome as the fourth argument after `stop_reason`. Pass the canonical assessment
projection, or JSON null when no assessment was performed. Harn callers constructing an
`AgentResult` must include its nullable `goal_outcome` field.
