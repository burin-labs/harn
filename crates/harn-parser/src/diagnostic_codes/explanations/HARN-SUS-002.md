# HARN-SUS-002 — ResumeConditions validation failed

`ResumeConditions` did not match the supported shape for a suspended agent.
The object may contain `trigger`, `timeout`, and `on_event`. Trigger entries
must parse as trigger specs, timeout entries need a positive
`duration_minutes`, and event entries must be valid runtime event topics.

Validate untrusted condition tables with `parse_resume_conditions(...)` or
`agent_await_resumption(...)` before passing them to `suspend_agent(...)`.
