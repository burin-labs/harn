- **Command policy can now REQUEST CONSENT instead of only allow/deny.** A
  `command_policy({...})` may carry a `consent` closure — the
  `std/llm/tool_middleware::with_consent` prompt_fn contract (`true` /
  `{decision: "approved"}` to allow, `false` / `{decision: "denied", reason?}`
  to deny). When a command lands on a `require_approval` risk class (a
  deterministic risk label listed in `require_approval`, or a pre-hook
  `require_approval` decision), it now routes through the consent gate instead
  of hard-denying: an approval lets the command run, a denial returns a
  `status: "consent_denied"` envelope without ever spawning a child process.
  The consent closure receives the command context enriched with
  `consent.reason` and `consent.risk_labels` and may call `request_approval` /
  `ask_user` to block on a human. Policies without a `consent` closure keep the
  legacy hard-block behavior byte-for-byte, so the default path is unchanged.
