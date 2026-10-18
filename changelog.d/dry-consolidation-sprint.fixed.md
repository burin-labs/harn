- **An exited, unreaped process no longer counts as alive.** On Linux, host-lease
  recovery treated a zombie lease owner as live, so under an init that does not
  reap orphans (common in containers) the lease could never be recovered. The
  probe now reports such a process as dead, matching the Windows probe, and is
  public as `harn_hostlib::process_liveness` so tests ask it instead of
  re-deriving liveness from `kill(pid, 0)`.
