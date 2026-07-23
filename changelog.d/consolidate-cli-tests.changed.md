- Internal: the `harn-cli` crate's ~92 separate integration-test binaries are
  consolidated into two — `harn_cli_fast` (the in-process suite the nextest
  `default`/`ci` profiles run) and `harn_cli_e2e` (the subprocess-spawning
  suite). Each former `tests/<name>.rs` is now a submodule under
  `tests/harn_cli_fast/` or `tests/harn_cli_e2e/`. Linking two binaries instead
  of 92 cuts test link time and shrinks the nextest archive. The nextest
  `default`/`ci` `default-filter` now selects the fast suite with
  `binary(harn_cli_fast)` in place of the 15-binary allowlist; the `e2e` profile
  keeps running `package(harn-cli) and kind(test)`, i.e. both binaries. No test
  was added or removed.
