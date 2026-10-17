- **One owner for duplicated runtime, CLI, and test policy.** The `harn dump-*`,
  codegen, `session schema`, and provider-artifact generators share one
  write-or-`--check` helper: stale and missing reports read the same
  everywhere, every comparison ignores line endings, and an already-current
  artifact is left untouched instead of rewritten. File errors now read
  `cannot write …`/`cannot create …`, and `harn bench replay` read errors read
  `failed to read …`. The CLI's MCP HTTP transport reuses harn-serve's
  streamable-HTTP helpers, now public as
  `harn_serve::adapters::mcp::streamable_http`. SQLite and Postgres stdlib share
  mock, savepoint, handle, and migration plumbing; event-log backends share one
  idempotent append; lifecycle, pool, and channel tracing share one span guard.
  `harn_vm::process_sandbox::host_contains_process_reads()` exposes the kernel
  procfs-containment measurement the sandbox already used.
