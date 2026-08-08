# Update Harn's ACP integration

This guide is for maintainers changing Harn's ACP server, ACP LLM provider, or
Harn-owned ACP extensions. For the public API, see
[MCP, ACP, and A2A integration](../mcp-and-acp.md). For the current SDK
decision, see [ADR 0006](../adr/0006-acp-hand-rolled-vs-sdk.md).

## Current boundary

Harn still owns the ACP runtime. Do not add `agent-client-protocol` as a
production dependency until ADR 0006's revisit checklist is green, including an
unmodified Zed session against an SDK-backed `harn serve acp`.

When that cutover lands, mirror the MCP ownership split in
[Update Harn's MCP integration](./mcp-maintenance.md):

- SDK owns protocol mechanics: typed messages and capabilities, version
  negotiation, JSON-RPC framing, request association, and transport shutdown.
- Harn owns product policy: VM conversion, session workspace anchors, modes,
  model pin, thought level, budget, forking policy, inject/remind, timeline,
  approvals, and audit events.

Until then, treat the official SDK schema as an oracle for drift checks, not as
the runtime.

## Owning paths

- `crates/harn-serve/src/adapters/acp/` for the serve adapter
- `crates/harn-vm/src/llm/providers/acp.rs` for the ACP LLM provider
- `crates/harn-cli/src/acp/` and
  `crates/harn-cli/src/commands/orchestrator/listener/acp_hub.rs` for CLI and
  multi-client hub entry
- `docs/src/spec/harn-extensions/v1.md` for Harn-owned extension fields
- `spec/acp-registry/` for registry submission artifacts

## Extension rules

- Prefer stable upstream ACP methods when they exist.
- Prefer `_`-prefixed method names for new Harn-only RPC
  ([ACP extensibility](https://agentclientprotocol.com/protocol/extensibility)).
  Existing `harn.*` methods are compatibility surface; do not grow that prefix
  without a migration plan.
- Keep `_meta.harn.*` product metadata on open meta maps. Do not fork upstream
  types only to rename fields.
- Custom `sessionUpdate` kinds that are not in the upstream enum need an
  explicit untyped or `_`-prefixed escape hatch before any SDK typed path owns
  emission.

## Prove changes

Start with the narrow ACP suite that covers the changed seam, then broaden:

```sh
make check-docs-snippets
make check-drift
```

Editor-host claims still need the manual smoke path in
[ACP editor hosts](../acp-editor-hosts.md) until an automated unmodified-client
suite exists.
