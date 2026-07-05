Workflow stages can now run a caller-supplied `executor` closure as their leaf instead of spawning a delegated
worker. Pass `executor: { ctx -> ... }` to `workflow_run_repair` (or set `executor` on any stage node) to wrap
harn's retry-with-feedback / verify / attempt-recording machinery around a bespoke in-process agent loop. The
closure receives `{task, attempt, prior_findings, prior_verification, prior_text, artifacts}` and returns
`{result | text, artifacts?, transcript?, verification?}`; failing attempts thread their findings into the next
call exactly like the delegated path. Omitting `executor` keeps the existing delegated-worker leaf unchanged.
