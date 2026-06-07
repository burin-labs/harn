- **Incremental project scans now detect same-instant edits that the
  modification-time heuristic alone misses.** `scan_incremental`'s automatic
  delta computation (the path taken when no explicit `changed_paths` signal is
  supplied) compared only `mtime > previous_mtime`. Millisecond mtime
  granularity collides on same-turn/same-second writes — and on
  coarse-granularity filesystems — so a file an agent wrote and then re-scanned
  in the same instant was silently treated as unchanged, leaving the index
  serving pre-edit symbol facts and feeding fuzzy-match-stale loops on weak
  local models. The delta now also flags a file as modified when its byte size
  differs from the cached record, an mtime-independent signal that catches the
  overwhelmingly common add/remove edit for free (the file metadata is already
  read for the mtime check). Length-preserving same-instant edits still rely on
  the explicit `changed_paths` bypass the agent loop already threads through
  after its own writes.
