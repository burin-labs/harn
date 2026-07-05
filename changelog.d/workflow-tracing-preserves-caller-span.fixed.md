- **Nested workflow runs no longer clobber the caller's trace.** Running
  `workflow_execute` / `workflow_run_repair` inside an enclosing timing span
  (e.g. a run-level `start_timing` / `timed(...)` held across the run) no longer
  resets the tracing collector out from under the caller. Previously the
  enclosing span was stranded — its `end_timing` threw
  `__timing_end: unknown timing handle` — and already-completed sibling spans
  were erased. Tracing is now reset only when the collector is idle, so the
  standalone case is unchanged while nested runs preserve the caller's spans.
