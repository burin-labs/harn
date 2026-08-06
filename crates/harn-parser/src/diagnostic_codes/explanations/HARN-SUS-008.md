# HARN-SUS-008 — resume timeout action is unsupported

An auto-resume timeout used an `on_timeout` action Harn does not implement.
Supported actions are `resume_with_summary`, `resume_with_input`, and `fail`.

Use `parse_resume_conditions(...)` to normalize timeout settings before
suspending, or update the timeout table to one of the supported actions.
