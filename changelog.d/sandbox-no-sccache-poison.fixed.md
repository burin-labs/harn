- **Sandboxed builds no longer poison the shared `sccache` daemon.** `sccache`
  runs as a single long-lived per-user server; if a sandboxed cargo build was the
  first caller to spawn it, the daemon inherited harn's `sandbox-exec`
  confinement permanently (even after reparenting to launchd) and then failed
  *every* later build machine-wide with `Operation not permitted` — unable to
  read build inputs outside the sandbox root or write its cache dir under
  `~/Library/Caches`. Sandboxed process spawns now bypass the rustc wrapper
  (empty `CARGO_BUILD_RUSTC_WRAPPER` / `RUSTC_WRAPPER`, which overrides
  `build.rustc-wrapper` in `.cargo/config.toml`), so a per-command sandbox can
  never confine the cross-workspace daemon. The on-disk cache and all
  unsandboxed builds are unaffected.
