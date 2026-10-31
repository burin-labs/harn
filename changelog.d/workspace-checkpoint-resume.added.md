- **Workspace checkpoints survive a host restart.** Each checkpoint session
  records its boundaries, attributed calls, and revision beside its stored
  files. `session/load` on an opted-in session that the process does not hold
  resumes that history, so `plan` and `restore` work against boundaries an
  earlier process captured. Embedders get `workspace_checkpoint::resume` and
  `workspace_checkpoint::unload`.
