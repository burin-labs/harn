- Rust embedders share capability ceilings, sandbox and stage policy, tool annotations,
  and credential-path classification through `harn-execution-policy`. Existing
  `harn-vm` public paths re-export the same types; serialized policy is unchanged.

  Migration: add an exact-version `harn-execution-policy` dependency when naming
  these types directly or updating code that relies on their defining crate.
