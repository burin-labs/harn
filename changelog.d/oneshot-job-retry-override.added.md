- **One-shot `@job` drivers can override or disable the retry policy.**
  `harn_serve::run_job_once_with_options(..)` accepts a new
  `harn_serve::JobRunOptions` whose `retry_override` replaces the `@job`'s
  declared `@retry`/`retry:` policy for that run only. `JobRunOptions::fail_fast()`
  runs a single attempt with no backoff sleep — the natural choice for one-shot
  CLI and failure-path test drivers, which previously inherited the `@job`'s
  multi-hour `svix` backoff and could hang for an hour-plus on an erroring job.
  Strictly opt-in: `run_job_once` / `run_job_once_with` (and the server path)
  are unchanged and still honour the `@job`'s declared policy when no override
  is given.
