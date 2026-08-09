# HARN-SUS-009 — resume input failed agent_loop input validation

`resume_agent(...)` received resume input that could not be converted into a
non-empty task prompt for the resumed agent loop.

Pass `nil` when resuming without new input, or pass a non-empty string or value
whose string form is suitable as the next task prompt.
