# Update Harn's MCP integration

This guide is for maintainers changing Harn's MCP client, either MCP server,
or the generated protocol contract. It covers the update workflow. For the
public API, see [MCP, ACP, and A2A integration](../mcp-and-acp.md). For the
design boundary, see [ADR 0003](../adr/0003-mcp-hand-rolled-vs-rmcp.md).

## Start from the upstream release

Use released MCP specifications and a released `rmcp` crate. Do not implement a
proposal or release candidate on the default path.

1. Read the [MCP changelog][mcp-changelog] for the target version.
2. Check the [official Rust SDK release][rmcp-release] and its Rust version,
   license, enabled features, and supported protocol versions.
3. Name one observable behavior that would disprove the planned cutover. For a
   lifecycle change, a useful falsifier is an official SDK client that cannot
   discover and call a real Harn server.

[mcp-changelog]: https://modelcontextprotocol.io/specification/2026-07-28/changelog
[rmcp-release]: https://github.com/modelcontextprotocol/rust-sdk/releases

## Keep the ownership boundary narrow

`rmcp` owns protocol mechanics: typed messages and capabilities, lifecycle and
version negotiation, JSON-RPC framing, request association, standard headers,
task and multi-round-trip types, protocol errors, and transport shutdown. Its
stdio service also drives standard prompt and resource multi-round trips.
Delete Harn code that duplicates those mechanics when the SDK provides them.

Stable server-side interaction uses multi-round-trip results. A handler that
needs roots, sampling, or elicitation returns `input_required`; the client
resolves the embedded requests and retries the original operation with
`inputResponses`. Do not recreate the removed live server-request channel.
Harn's shared client boundary applies that re-entry policy to stable HTTP and
composes tool multi-round trips with task polling; those are Harn-owned seams
that the SDK does not provide as one operation.

Harn owns product policy: VM conversion, egress and SSRF controls, delegated
token exchange, interactive OAuth recovery, capability fixtures, proxy
routing, audit events, orchestrator dispatch, and persistence. Keep an adapter
only when it enforces one of those contracts. Do not fork an SDK type merely to
rename its fields.

Harn-owned servers implement the current stable version only. Do not add an
`initialize` fallback, session header, legacy SSE route, or old request shape
to a server adapter. The SDK-managed stdio client is the compatibility boundary
for released older servers.

The owning paths are:

- `crates/harn-vm/src/mcp/sdk.rs` for the SDK client handler;
- `crates/harn-vm/src/mcp_input.rs` for Harn handler suspension and stable MRTR re-entry;
- `crates/harn-vm/src/mcp_protocol.rs` for Harn policy projected from SDK types;
- `crates/harn-vm/src/mcp/transport.rs` for Harn-specific HTTP policy;
- `crates/harn-serve/src/adapters/mcp.rs` for the generic server;
- `crates/harn-cli/src/commands/mcp/serve/` for the orchestrator server;
- `crates/harn-mcp-compat/` for official-SDK and wire interoperability;
- `conformance/protocols/schemas/` for pinned upstream schemas; and
- `crates/harn-cli/src/commands/dump_protocol_artifacts/` for generated
  downstream projections.

## Refresh the contract

1. Update the workspace dependency and select only the SDK features Harn uses.
2. Update `mcp_protocol.rs`. Public string constants may remain where generated
   bindings require `&'static str`, but tests must compare them with the SDK's
   typed registry.
3. Fetch the released upstream schema using the `refresh_command` recorded in
   its `x-harn-provenance`. Preserve the upstream `$defs` exactly. Add only the
   Harn profile root and Harn-owned extension definitions.
4. Update the generator, not generated TypeScript, Swift, Rust, Python, or Go
   files.
5. Regenerate every projection:

```sh
make gen-protocol-artifacts
```

1. Search the MCP owning paths for the former version, proposal labels, and
   temporary aliases. Historical changelog entries are records and should not
   be rewritten.

## Prove the cutover

Run the narrow protocol gates first:

```sh
make mcp-conformance
make protocol-conformance
make check-protocol-artifacts
make lint-harn
make fmt-harn
```

The compatibility suite must include a real `rmcp` client against a real Harn
HTTP server. Fake peers remain useful for exact error and recovery cases, but
they do not prove SDK interoperability.

Then run the repository gates required by the changed surfaces:

```sh
make check-docs-snippets
make check-drift
make check-drift-binary
make all
```

Inspect `git status` after generation and after the final gate. A clean result
requires both runtime evidence and checked-in projections with no drift.
