- The `main health` commit status now appears on main's current head. Each push
  to main carries the latest verdict forward, keeping the date of the run that
  measured it, instead of leaving it on whichever commit was main's head at the
  daily run. The watched suites and their failure thresholds are declared in
  `scripts/scheduled_workflows.toml`, and every scheduled workflow must be
  either watched or exempt with a reason. The consumer canary, the weekly
  provider probe, stack-frame banking, the Actions vulnerability audit, and the
  cache and release controllers are now watched.
