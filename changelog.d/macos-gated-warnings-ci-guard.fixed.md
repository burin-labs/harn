- **macOS-gated `#[cfg(target_os = "macos")]` warnings can no longer slip to
  `main` and break the release `prepare` build.** The Linux per-PR CI lanes
  never compile macOS-only code, so a stray unused import / dead_code /
  deprecation in a `target_os = "macos"` (or `cfg(any(macos, windows))`) path
  only surfaced on a contributor's Mac — historically at release `prepare`
  time under `-D warnings`, one error at a time (the v0.8.109 blocker was an
  unused `BTreeMap` import in `crates/harn-hostlib/tests/secret_store_os_native.rs`,
  a `#![cfg(any(macos, windows))]` test file Linux CI skips). Removed that
  import and added a path-routed `macos` CI lane (analogue of the existing
  `windows` lane) that runs `cargo clippy --workspace --all-targets -D
  warnings` on `macos-latest` for PRs touching macOS-gated process/sandbox/
  secret-store/CLI paths, plus unconditionally on push/merge — compiling even
  the cfg-gated *test* targets so the class fails the PR instead of the
  release.
