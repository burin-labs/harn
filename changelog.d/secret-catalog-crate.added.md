- Extracted the canonical secret pattern catalog into a new dependency-free
  `harn-secret-catalog` crate so off-runtime host consumers can share the single
  source of truth instead of forking their own detector lists. `harn-vm`'s
  redaction and `secret_scan` paths now re-export it with byte-identical
  behavior.
