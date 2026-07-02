- **Subprocesses now die with their invoking scope.** Foreground `run_command` /
  `run_test` / `run_build_command` / `manage_packages` tool commands and the
  VM-side `process.exec` / `shell` / `exec_opts` builtins spawn their child in
  its own process group and, when the invoking scope is cancelled, a `deadline`
  expires, or the VM is dropped, terminate the whole group — SIGTERM, a 2s grace
  period, then SIGKILL (Unix; best-effort direct-child kill on Windows).
  Previously such children (and their grandchildren) kept running as orphans
  until they exited on their own. Scripts that relied on an orphaned survivor
  should use the existing background form instead:
  `run_command({..., background: true})` children are exempt from scope
  cancellation and are reaped only via `cancel_handle` or agent-session-end
  cleanup. As part of the same change, `deadline`/host-cancel now preempt a
  *blocking* command mid-wait (the command returns `status: "killed"` and the
  scope error surfaces immediately) instead of waiting for the child to finish.
