# HARN-SUS-001 — suspend_agent target worker is not running

`suspend_agent(...)` can only create a new cooperative suspension for a
running worker. A worker that is awaiting input, completed, failed, cancelled,
or interrupted no longer has a running turn boundary where a suspend checkpoint
can be installed.

Use the worker summary status to branch before suspending. For terminal workers,
spawn a fresh worker instead. Calling `suspend_agent(...)` again on an already
suspended worker remains an idempotent summary read.
