- `CodeIndexCapability::warm_session` now restores and reconciles an on-disk
  snapshot on the background warm thread and returns `Building` at once. A
  stale snapshot on a large repository no longer blocks the embedder's session
  start for minutes. `SessionWarmOutcome::Restored` is removed. (#9272)
