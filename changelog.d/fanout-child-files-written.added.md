Sub-agent result envelopes (and therefore `agent_fanout` per-child results) now
carry `files_written` — the authoritative set of workspace paths the child
actually mutated through the deterministic hostlib write surface, collected at
the single `fs_snapshot::auto_capture_for_write` chokepoint so capability-denied
out-of-scope writes are excluded — plus a `usage` object
(`input_tokens`/`output_tokens`/`total_tokens`). This lets a fan-out parent
attribute writes to each child and detect a child that claimed completion but
wrote nothing or wrote outside its scope, without re-parsing transcripts, and
works headless. Exposed via new
`harn_vm::agent_sessions::{record,session,take,clear}_session_changed_path(s)`
helpers fed from the hostlib write chokepoint.
