- **Workspace checkpoints resume safely after a host restart.** Each session
  records its boundaries, attributed calls, and revision beside its stored
  files. A tracked tool runs only after an incomplete index is durably written;
  a restart during that call is refused on resume instead of restoring stale
  history. Completed calls and restores publish their final timeline before
  the index becomes complete. Embedders get `workspace_checkpoint::resume`
  and `workspace_checkpoint::unload`.
