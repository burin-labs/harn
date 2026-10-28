- Route the agent-state store, OAuth token file, signal inbox, daemon
  snapshots and metadata, check-result cache, batch fixture receipts, and the
  source gate receipt through the shared atomic-write helper, so each is
  fsynced, replaced atomically, and (for OAuth) created owner-only. A workspace
  test now fails when a new hand-rolled temp-file-and-rename appears.
