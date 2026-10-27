- Orchestrator JSON logs now go to stderr instead of stdout. `harn run` and
  `harn bench` report concise errors by default.

  Migration: Redirect stderr when consuming orchestrator JSON logs, and pass
  `--explain` to recover source excerpts and runtime stack details.
