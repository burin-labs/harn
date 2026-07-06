The hostlib `run_command` builtin now consults host mocks before spawning, so
command execution is deterministically mockable in tests. A `("process",
"run_command")` mock matches directly; a legacy `("process", "exec")` mock
matches as a fallback, so suites that mock the `process.exec` host-call seam
transparently cover commands that route through hostlib `run_command` without
being rewritten. Exposed as `harn_vm::consult_command_execution_mock`.
