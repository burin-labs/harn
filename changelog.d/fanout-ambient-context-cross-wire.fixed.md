- Fixed two SEV-1 fan-out concurrency cross-wires: the per-worker VM execution
  context (cwd/env/source-dir + capability path-scope root) and mutation session
  (audit/run_id/approval/secret-scope) are now captured into
  `AmbientExecutionScope` and swapped per-poll, so cooperatively-scheduled
  `spawn_local` children no longer read a sibling's worktree root, environment,
  or audit/secret attribution across an `.await`. A drift guard now fails CI if a
  new ambient-shape thread-local is added without classifying it.
