- `parallel` and `parallel each` are now fail-fast: the first branch that
  throws cancels all in-flight siblings (in-flight LLM/host calls are dropped,
  queued branches never start) and its error propagates out of the construct.
  Previously every branch ran to completion and the first error in source
  order was raised only after all branches finished. When several branches
  have already failed by the time the cancellation lands, the lowest-index
  branch's error is reported, so the propagated error stays deterministic.
  Cancelled siblings are still joined before the construct returns.
  **Migration:** if you need every branch to run regardless of failures,
  switch to `parallel settle`, which is unchanged and still runs everything,
  collecting per-branch `Ok`/`Err` outcomes.
