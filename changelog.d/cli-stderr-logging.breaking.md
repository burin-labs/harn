- Orchestrator JSON logs now go to stderr instead of stdout. Redirect stderr
  when consuming those logs. `harn run` reports concise errors by default;
  pass `--explain` for source excerpts and runtime stack details.
