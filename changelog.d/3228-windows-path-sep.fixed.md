- `run()` toolchain-PATH normalizer (`HARN_RUN_TOOLCHAIN_PATH`) now builds the
  child `PATH` with `std::env::join_paths`/`split_paths`, so prepend/override/
  replace use the platform separator (`;` on Windows, `:` on unix) instead of a
  hardcoded `:`. The PATH env key is also matched case-insensitively on Windows
  (`Path`/`PATH`) so an existing caller-supplied key is updated in place rather
  than duplicated. Fixes the red "Rust on Windows" CI job.
