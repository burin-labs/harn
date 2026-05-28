- **`harn-serve`: serialize `mcp_host_bridge` tests that mutate the
  process-global MCP host guard.** `install_with_none_clears_guard` and
  `tool_filter_only_admits_listed_tools` both call
  `mcp_host::reset_for_tests()` and then install an allowlist via
  `set_allowlist`. Under `cargo test -p harn-serve --lib` (~309 tests,
  parallel), a neighbour could race the install→assert window and flip
  the global guard mid-test, producing a ~50% flake rate. Both tests now
  take a poison-tolerant `static Mutex<()>` for their duration, pinning
  the global to one test at a time.
