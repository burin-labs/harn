- **Self-deadlock detection (`HARN-ORC-011`) now spans inline async-builtin
  boundaries.** Acquiring a `mutex` that an ancestor already holds — when that
  ancestor is parked awaiting the async builtin or closure you're running inline
  — is a provably-unresolvable self-deadlock (the sole holder is blocked waiting
  on you). The VM now propagates an ancestor's held-lock keys into such inline
  children and raises `HARN-ORC-011` instead of hanging forever. New concurrent
  tasks (`spawn`, `parallel`, triggers) deliberately do *not* inherit, since
  blocking on a parent-held lock there is legitimately resolvable.
