- `std/fs` gains `fs_snapshot(paths, opts?)`, `fs_restore(snapshot_id, paths?, opts?)`, `fs_list_snapshots(opts?)`, and
  `fs_drop_snapshot(snapshot_id, opts?)` — thin pipeline-facing wrappers over the existing host
  `hostlib_fs_*` snapshot builtins. Pipelines can now checkpoint the workspace and roll a mutation back between
  independent retry attempts (the substrate for a verify-gated best-of-N agent loop). Snapshots are session-scoped:
  the session id defaults to the active agent session (`agent_session_current_id()`) so each conversation's snapshots
  stay isolated and are cleaned up on session close.
