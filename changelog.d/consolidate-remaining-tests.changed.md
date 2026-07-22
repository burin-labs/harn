- Test infra: the integration-test suites of `harn-serve`, `harn-rules`,
  `harn-mcp-rc-compat`, `harn-session-store`, and `harn-codegen` are each
  consolidated from their per-file `tests/*.rs` binaries into a single
  `tests/<crate>/main.rs` binary per crate (26 integration binaries down to
  5), continuing the link-time and `cargo nextest` archive-size reduction
  landed for `harn-hostlib` (#5427) and `harn-vm` (#5432). Every leaf test is
  preserved unchanged; integration test names now carry their former file
  name as a module prefix (e.g. `site_auth::…`). No product behavior change.
