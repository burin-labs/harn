Run-tape hosts can declare durable storage outside their workspace. Replay redirects writes under recorded and current
host roots into isolated branch state, including hosts that create a new home for each run.

Migration:

Rust hosts constructing `TapeWorkspace` must set `state_roots`, and hosts constructing `BranchPlan` must set
`host_state_roots`. Use an empty vector when the host has no additional durable roots; otherwise supply its storage
directories for that execution.
