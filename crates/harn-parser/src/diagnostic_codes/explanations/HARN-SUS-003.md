# HARN-SUS-003 — resume_agent target worker is not suspended

`resume_agent(...)` was called on a live worker that is not currently in the
`suspended` state. Warm resume is only meaningful for workers with an active
suspension envelope.

Check the worker summary before resuming. Use `send_input(...)` for workers
that are awaiting input, inspect terminal summaries directly, or load a
snapshot path when you only need to restore persisted state.
