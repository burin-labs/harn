- **`files_written` on the fan-out path.** A sub-agent's edits are no longer
  dropped from its receipt when the agent loop dispatches the turn's tool calls
  through `parallel` / `parallel settle`. Those subtasks now run under an
  isolated copy of the spawning agent's full ambient scope (session, execution
  context, mutation session, policies), so a dispatched tool's write attributes
  to the agent's session even while a fan-out worker's scope is swapped out.
  Previously the receipt reported `files_written: []` for a child that really
  did edit files, which a downstream host renders as "wrote 0 file(s)" /
  "0/N units completed" and can trigger wasteful parent re-work.
