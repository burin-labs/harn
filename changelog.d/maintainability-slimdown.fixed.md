- **Agent session redo checkpoints survive rejected transcript-budget writes.** Failed
  transcript mutations that leave the transcript unchanged no longer discard redo
  state captured by the previous rollback.
