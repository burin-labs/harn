# ADR 0006: keep hand-rolled ACP until the official SDK clears the Zed falsifier

## Status

Accepted on 2026-08-05 for
[#6088](https://github.com/burin-labs/harn/issues/6088). **Decision: keep the
hand-rolled ACP implementation; do not adopt `agent-client-protocol` as a
runtime dependency yet.**

The official Rust SDK is Apache-2.0, Agent-capable, and transport-complete
enough to own commodity protocol mechanics the way `rmcp` now owns MCP
([ADR 0003](./0003-mcp-hand-rolled-vs-rmcp.md)). Adoption is the intended
destination. The current call is that the cutover is not yet proven against the
stated falsifier, and several Harn extension seams need an explicit escape hatch
before deleting the adapter.

## Context

[#6072](https://github.com/burin-labs/harn/pull/6072) cut Harn's MCP client and
servers over to `rmcp` 3.1. The same question is open for ACP.

Harn currently ships a bespoke ACP stack. A 2026-08-05 survey of this checkout
put the serve adapter at about **11k non-test lines** in
`crates/harn-serve/src/adapters/acp/` (about 24k including tests), plus a
stdio ACP client/provider in `crates/harn-vm/src/llm/providers/acp.rs` (~0.6k
non-test) and the orchestrator WebSocket hub in
`crates/harn-cli/src/commands/orchestrator/listener/acp_hub.rs` (~2.2k). There
is no `agent-client-protocol` Cargo dependency. Harn advertises
`agentclientprotocol/agent-client-protocol schema v0.12.2`
(`ACP_SCHEMA_COMPATIBILITY` in `crates/harn-serve/src/adapters/acp/schema.rs`).

An official Rust SDK now exists at
[agentclientprotocol/rust-sdk](https://github.com/agentclientprotocol/rust-sdk).
Verified on crates.io 2026-08-05:

| Crate | Version | License | MSRV | Notes |
|---|---|---|---|---|
| `agent-client-protocol` | 2.0.0 | Apache-2.0 | 1.88 | Agent and Client roles; pins `agent-client-protocol-schema =1.5.0` |
| `agent-client-protocol-http` | 2.0.0 | Apache-2.0 | 1.88 | HTTP/SSE and optional WebSocket transports |
| `agent-client-protocol-rmcp` | 3.0.0 | Apache-2.0 | 1.88 | ACP↔MCP bridge; depends on **`rmcp ^2.1`** |

Harn's toolchain is newer than the SDK MSRV. Harn's MCP stack uses **`rmcp`
3.1.0**, so `agent-client-protocol-rmcp` cannot join the workspace until that
bridge tracks `rmcp` 3.x. Core ACP adoption does not require the bridge crate.

## Falsifier

The disproof for a cutover commit is:

> An unmodified Zed (or another official ACP client) cannot complete a session
> against `harn serve acp` built on the SDK.

If the SDK cannot express Harn's session extensions without forking SDK types,
keep the adapter and leave this ADR as the record. That type-fork condition is
not met today: open `_meta`, Harn-owned `JsonRpcRequest` handlers, and
`UntypedMessage` cover the product extensions. The Zed session falsifier has
not been run against an SDK-backed agent, so the cutover remains blocked.

## Survey

### What the SDK owns well

- Agent role (`role::acp::Agent`, cookbook `building_an_agent`, `examples/simple_agent.rs` over `Stdio`)
- JSON-RPC framing, request association, cancellation, and connection lifecycle
- Typed stable methods including `initialize`, `authenticate`, `session/new`,
  `session/prompt`, `session/cancel`, `session/load`, `session/resume`,
  `session/close`, `session/list`, `session/set_mode`,
  `session/set_config_option`, and `session/fork` (`ForkSessionRequest` is in
  the published `ClientRequest` enum)
- Stdio transport in the core crate; HTTP/SSE and WebSocket in
  `agent-client-protocol-http`
- Protocol version negotiation (`ProtocolVersion::V0` / `V1`; draft v2 behind
  `unstable_protocol_v2`)

### What Harn must keep

These stay Harn-owned under any adoption path. They are product policy, not
protocol plumbing:

- VM conversion, prompt execution, sandbox and capability policy
- Session workspace anchors, modes, model pin, thought level, budget
- Inject/remind queues, checkpoints, staged filesystem, HITL, workflow control
- Session timeline / session view projections and EventLog audit
- MCP-over-ACP control plane and orchestrator multi-client hub policy
- `_meta.harn.*` product metadata

### Extension seams that block a naive types wipe

1. **Schema pin drift.** Harn still advertises schema **v0.12.2**. The SDK pins
   schema **1.5.0**. Field and capability shape changes on the Zed path must be
   proven before deleting Harn's wire model.
2. **Extension method names.** ACP reserves underscore-prefixed methods for
   extensions. Official `ExtRequest` documents that extension method names must
   start with `_`. Harn already ships `_harn/*` notifications, but also
   `harn.session_*`, `harn.mcp.*`, and `harn.workflow.*` method names that are
   outside that reservation. Those need Harn-owned handlers (or a rename to
   `_harn/*`) rather than `ClientRequest::ExtMethodRequest`.
3. **Closed `SessionUpdate`.** The SDK enum covers the stable update kinds
   only. Harn emits additional `sessionUpdate` values such as `artifact`,
   `progress`, `hitl_request`, and `reminder_emitted`. Typed
   `SessionNotification` emission cannot carry those without an untyped or
   `_`-prefixed escape hatch.
4. **`agent-client-protocol-rmcp`.** Useful signal that ACP and MCP SDKs are
   meant to compose, but its `rmcp ^2.1` pin conflicts with Harn's `rmcp` 3.1.
   Do not pull it into the first cutover.

### Duplicated mechanics (deletion candidates after the falsifier passes)

Mirror the MCP deletion set once an SDK-backed Agent clears Zed:

- JSON-RPC writers and pending-id association (`io.rs`, parts of `transport.rs`,
  `dispatch.rs`, bridge waiters)
- Standard wire types for stable methods (`types.rs` share that matches schema
  1.5.0)
- Stdio and commodity WebSocket framing loops (keep TLS, bind, auth, and hub
  policy adapters)
- Client-side stdio agent driver framing in `providers/acp.rs` if the SDK client
  path replaces it

## Decision

1. **Keep the hand-rolled ACP runtime** as the production path for
   `harn serve acp` and the ACP LLM provider.
2. **Treat `agent-client-protocol` 2.x as the target oracle** for stable method
   names, error codes, and schema evolution. Prefer drift checks against the
   SDK schema over inventing a second protocol model.
3. **Ownership split after adoption** (same shape as ADR 0003 / MCP maintenance):
   - SDK owns protocol mechanics: message types, capability and version
     negotiation, JSON-RPC framing, request association, transport shutdown.
   - Harn owns product policy: VM conversion, session workspace anchors, session
     modes, model pin, thought level, budget, forking policy, queued user
     messages, typed pipeline returns, approval routing, and audit events.
4. **Do not fork SDK types** to carry Harn extensions. Use open `_meta.harn`,
   Harn-owned request handlers for `harn.*` / `_harn/*`, and untyped emission
   for non-standard `sessionUpdate` kinds until those kinds are upstream or
   renamed under the `_` reservation.
5. **Defer `agent-client-protocol-http` and `agent-client-protocol-rmcp`** until
   the stdio Agent path is green. Harn's production editor path is stdio; HTTP
   and the rmcp bridge are follow-on transports.

## Revisit → adopt when

Flip this ADR to "use the official ACP SDK" (and supersede this status) only
when all of the following are true:

1. A spike serves `initialize → session/new → session/prompt → session/cancel`
   from an SDK-backed Agent over stdio without forking SDK crates.
2. **An unmodified Zed completes that session** against the spike binary
   (falsifier green). Record the Zed build/version and the exact prompt path.
3. Harn's advertised schema compatibility is reconciled with the SDK schema
   oracle, with a registry or drift check similar to MCP's
   `protocol_registry_matches_official_sdk`.
4. The extension strategy is written into an ACP maintenance how-to: no SDK type
   forks; `harn.*` / `_harn/*` handlers and custom updates remain Harn-owned.
5. Workspace dependency selection excludes `agent-client-protocol-rmcp` until it
   supports `rmcp` 3.x, or Harn deliberately dual-stacks with a documented
   reason.

## Consequences

- No ACP SDK dependency lands in this decision. The ~11k-line serve adapter and
  provider stay until the revisit checklist is complete.
- New ACP protocol mechanics should be designed for eventual SDK ownership:
  prefer stable upstream methods and `_`-prefixed extensions; avoid growing
  bespoke framing.
- Product extensions documented in
  [MCP, ACP, and A2A integration](../mcp-and-acp.md) and
  [Harn ACP/MCP extensions v1](../spec/harn-extensions/v1.md) remain Harn-owned
  and are not a reason to reject the SDK by themselves.
- Companion A2A evaluation is recorded in
  [ADR 0005](./0005-a2a-keep-bespoke-adapter.md)
  ([#6089](https://github.com/burin-labs/harn/issues/6089)).

## Evidence

- crates.io metadata for `agent-client-protocol` 2.0.0,
  `agent-client-protocol-http` 2.0.0, and `agent-client-protocol-rmcp` 3.0.0
  (2026-08-05): Apache-2.0, MSRV 1.88, schema pin `=1.5.0`, rmcp bridge on
  `^2.1`.
- docs.rs: Agent role, `ClientRequest` including `ForkSessionRequest`, closed
  `SessionUpdate`, `ExtRequest` requiring `_`-prefixed methods, stdio and
  cookbook agent path.
- Harn sources: `ACP_SCHEMA_COMPATIBILITY` = schema v0.12.2;
  `HARN_SESSION_UPDATE_EXTENSIONS` in `schema.rs`; `harn.*` methods in
  `docs/src/mcp-and-acp.md`; `rmcp = "3.1.0"` in `harn-vm` and
  `harn-mcp-compat`.
- No automated unmodified-Zed suite exists today; editor smoke remains manual in
  [ACP editor hosts](../acp-editor-hosts.md).

See [Update Harn's MCP integration](../dev/mcp-maintenance.md) for the MCP
ownership precedent this ACP cutover should mirror once the falsifier is green.
