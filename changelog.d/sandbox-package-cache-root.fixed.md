A sandboxed run now uses the same package cache as its host instead of an
empty one. The sandbox relocates `HOME` and `XDG_CACHE_HOME` into the workspace
so each toolchain writes its caches there, and Harn's own package cache was
following them. Every such run therefore resolved a cache that was empty by
construction and re-fetched packages the host had already materialized, into a
network the same sandbox denies. The only error that surfaced was the fetch's,
naming DNS or repository access rather than the relocation that caused it. The
resolved cache root is now handed to the child and granted, the way `CARGO_HOME`
and the Go caches already were.
