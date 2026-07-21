Prevented the stdio JSON-RPC test-hang class across the board: the ACP
(`serve acp`) and test-worker (`serve test`) process-e2e suites drove their
long-lived children with an unbounded `read_line` and no captured stderr, so a
wedged server consumed the entire nextest slow-test cap as an opaque 180s
`TIMEOUT` — the same footgun fixed for `mcp serve` in #5398. All interactive
stdio surfaces now share one bounded, self-diagnosing `StdioJsonRpcClient`
(`crates/harn-cli/tests/test_util/stdio_jsonrpc.rs`): every read is
deadline-bounded and every failure path kills the child and reports its stderr
plus the in-flight request. The separate `tests/support/` module is retired in
favor of the shared `test_util` owner.
