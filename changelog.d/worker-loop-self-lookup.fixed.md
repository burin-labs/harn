A delegated worker started from inside a tool call now runs its loop under that
tool's capability ceiling even when the ceiling grants no worker capability. The
loop used to find its own worker record by listing every delegated worker, which
the ceiling refused (`harness.agent.worker_list exceeds the active effect
ceiling`) before the worker's first model call. It now reads only its own record
through the new `harness.agent.current_worker()`, authorized like
`harness.agent.current_id()`. Listing workers still needs a worker capability.
