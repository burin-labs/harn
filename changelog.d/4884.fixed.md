- **Documented the sandbox and egress environment variables, and fixed a README link.**
  The environment-variable reference now covers `HARN_HANDLER_SANDBOX` (how the
  `worktree` profile reacts when OS confinement is unavailable) and the
  `HARN_EGRESS_ALLOW` / `HARN_EGRESS_DENY` / `HARN_EGRESS_DEFAULT` /
  `HARN_EGRESS_BLOCK_PRIVATE` / `HARN_EGRESS_ALLOW_LOOPBACK` egress and
  private-address-guard knobs, which were previously described only in scattered
  prose. The README's "approval policies" link pointed at the OS process-sandbox
  page instead of the approval-policy DSL, and now points at both.
