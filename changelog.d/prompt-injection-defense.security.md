- **Prompt-injection defense substrate (`[security]` config + `std/security`).** The runtime now
  spotlights untrusted external content and gates the lethal trifecta. Tool/MCP output that crossed a
  trust boundary (an external MCP server, or a `Fetch`-kind tool reaching the open internet) is framed
  in datamarked delimiters with a provenance banner so the model treats it as data, never as
  instructions (Microsoft "spotlighting"). A per-session taint ledger records that untrusted content
  entered context; when it has, an auto-allowed tool that can exfiltrate (network/fetch), destroy
  state, or read a secret file is upgraded to an interactive confirmation — but only where an approval
  policy is installed, so headless embedders are unaffected. MCP tool schemas are pinned and hashed on
  `tools/list`; a description/inputSchema that changes after first sighting is flagged
  (`_schema_changed`) for re-approval (rug-pull defense), and `session/request_permission` now carries
  the full tool descriptor so hosts can render the complete model-visible tool text at approval time
  (closing the tool-poisoning visibility gap). Configure via `[security]` (`mode = off | spotlight |
  strict | local-ml`, `trifecta_gate`, `pin_mcp_schemas`, `gate_secret_reads`, `trusted_mcp_servers`)
  or `std/security::configure`. Defaults are on (spotlight + gate + pinning).
