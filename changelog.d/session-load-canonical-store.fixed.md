- **`session/load` restores any session the canonical store holds
  (burin-labs/burin-code#6267).** Restorability was decided by replaying the
  observability event log, a best-effort sink that is registered only while a
  prompt runs and silently no-ops when no log is installed on the emitting
  thread. `session/list` has always answered from the canonical session store,
  so clients were handed session ids that `session/load` then rejected with
  `unknown session` — the durable transcript was on disk and unreachable. Load
  now falls back to the same store `session/list` reads, projecting its rows
  into the existing replay stream, and reports an unknown session only when no
  store holds the id.
