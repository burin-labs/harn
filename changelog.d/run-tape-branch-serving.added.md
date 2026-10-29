- **A branch prefix serves MCP servers, the capability probe and script-level
  process runs from its tape, and reports a verdict.** A run tape records each MCP
  server start (command and negotiated server info) beside its exchanges. In a
  branch prefix an agent's MCP server does not start: its exchanges are served from
  the recording. The server starts on the first exchange after the branch goes
  live, and a different server version or tool list is a named divergence. The
  self-hosted capability probe's answer is recorded on every use and served to the
  prefix. A process run outside any tool dispatch (`harness.process.run`, `exec`,
  `shell`, the git queries and `harness.tools.run_command`) is taped like a tool
  call, result and workspace changes, and served instead of refused.
  `BranchScope::run_bounded` ends a branch on a stop or a timeout, and
  `BranchGuard::verdict` returns a `harn.branch_verdict.v1` record with the run's
  status and each model call's fidelity: served, live or diverged.
