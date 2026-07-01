- **MCP HTTP transport now step-ups OAuth on a `403 insufficient_scope`
  challenge, not just a `401`.** Per RFC 6750 §3.1, a `403` whose
  `WWW-Authenticate: Bearer` header carries `error="insufficient_scope"` means
  the presented token is valid but lacks a required scope. Harn now treats it
  like a `401`: it emits `mcp_auth_required` (carrying the challenge's elevated
  `scope`) and re-runs the authorization flow, so a tool call that needs an
  additional scope recovers in place instead of dead-ending. A plain `403`
  without an `insufficient_scope` challenge is still a hard denial and falls
  through unchanged.
