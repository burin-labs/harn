- **49 standard-library builtin families moved out of `harn-vm` into the new
  `harn-builtins` crate.** Git, cookies, web, Postgres, SQLite, CSV, cron,
  and the other families that `harn-vm` never calls into now compile beside
  the VM instead of inside it, removing about 44,000 lines from the serial
  `harn-vm` build. The language prelude (types, strings, bytes, collections,
  iteration, math, sets, regex, crypto, and assertions), the families the
  embedded agent stdlib calls, and the connector and monitor builtins the
  trigger dispatcher drives stay in `harn-vm`. Script behavior is unchanged.

  Migration: replace `harn_vm::stdlib::<family>::` paths with
  `harn_builtins::<family>::` for the moved families, and
  `harn_vm::install_shared_pool_registry` with
  `harn_builtins::postgres::install_shared_pool_registry`. Select the optional
  families through `harn-runtime`'s `postgres`, `sqlite`, `content`, and
  `compression` features, or `full`. Crates whose tests need `harn-vm`'s test
  helpers enable its new `vm-test-support` feature.
