- Own the workspace `cargo-nextest` pin in `.github/cache-policy.json`
  (`nextest_version`, schema v4) and load it from CI scripts via
  `scripts/ci/cache_policy.sh`, so Windows warm artifacts, Rust test archives,
  and workflow `nextest@` installs share one config surface.
