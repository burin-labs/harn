- Corrected a stale `harn-secret-catalog` version pin in `Cargo.lock` (0.9.9 →
  0.9.10) so the workspace lockfile matches the current crate version and cargo
  no longer re-dirties the tree on build.
