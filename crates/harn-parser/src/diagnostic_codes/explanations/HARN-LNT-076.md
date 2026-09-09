# HARN-LNT-076 — tool handler reaches the privileged host wire

`host_call(...)` belongs to the trusted entry boundary selected by an embedding
host. A tool handler runs later under model or client control, where that wire
is not a stable capability: its bridge can be absent even when the same call
worked while assembling the pipeline.

Read the host-owned value before registering the handler and pass it through a
closure or an explicit typed capability. At runtime, reaching `host_call` from
a handler raises an error that names the unavailable operation; it never falls
through to a standalone default that can be mistaken for an empty host answer.
