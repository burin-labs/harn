- Orchestrator JSON logs now go to stderr instead of stdout. `harn run` and
  `harn bench` report concise errors by default. Common CLI diagnostics
  use the `[harn]` prefix, and test-watch progress stays on stderr. The test
  watcher no longer reruns itself when reading a test file.

  Migration: Redirect stderr when consuming orchestrator JSON logs, and pass
  `--explain` to recover source excerpts and runtime stack details.
