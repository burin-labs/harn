# Protocol contribution RFCs

This directory collects RFC-shaped documents that Burin Labs intends to
contribute to upstream agent-protocol working groups. Each RFC describes
a primitive that Harn already ships under a protocol's `_meta`
extensibility slot, so the upstream proposal can reference a running
reference implementation instead of starting from a design sketch.

## Filing pattern

The Harn convention for cross-protocol primitives is:

1. **Ship the reference implementation first** under the protocol's
   designated extensibility slot (`_meta` for ACP / MCP, the JSON-RPC
   envelope's `metadata` for A2A). This proves the shape end-to-end
   without disturbing upstream consumers.
2. **Author an RFC document in this directory.** Each RFC is written as
   a neutral standards-grade proposal: problem statement, wire-format
   schema, compatibility story, reference-impl status, and open
   questions for maintainers. The wire-format snippet matches the
   upstream repo's preferred dialect (TypeScript for ACP, JSON Schema
   for MCP, A2A JSON-RPC envelope for A2A).
3. **Open an upstream discussion** when the RFC is ready. Treat the
   RFC doc as source material, then do a public-filing pass before
   posting: lead with neutral protocol semantics, deployed peer
   behavior, and public prior art rather than Harn or Burin-specific
   adoption claims. Keep the scope fixed inside the filed thread so
   maintainers can triage one proposal at a time.
4. **Track outcomes here.** When a proposal lands upstream, update the
   RFC's "Status" header and migrate the Harn `_meta` envelope to the
   standardized field locations in a follow-up PR. When a proposal is
   declined, record the decision in the RFC and keep the `_meta` shape
   stable for downstream consumers.

## Current RFCs

Statuses below were last verified on 2026-07-03. See the
[filing status ledger](./status-ledger.md) for the upstream PR,
discussion, and issue states checked during triage.

### Ambient reminder injection ([#1829][1829])

| RFC | Upstream | Status | Reference impl |
|---|---|---|---|
| [ACP `session/inject_reminder`](./acp-session-inject-reminder.md) | agentclientprotocol/agent-client-protocol | Discussion open ([ACP #1224](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224)) | `session/remind` JSON-RPC method + `_meta.harn.reminder`-decorated transcript events |
| [A2A `InjectTaskReminder`](./a2a-message-kind-reminder.md) | a2aproject/A2A | Discussion open ([A2A #2027](https://github.com/a2aproject/A2A/discussions/2027), filed 2026-07-03); revised to A2A v1.0 conventions 2026-07-03 | `metadata.harn.reminder` on outbound A2A task events |
| [MCP `notifications/reminder`](./mcp-notifications-reminder.md) | modelcontextprotocol/modelcontextprotocol | Discussion open ([MCP #3007](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3007), filed 2026-07-03) | `_meta.harn.reminder` on MCP server-emitted notifications |

### Suspend / resume + paused state ([#1848][1848])

| RFC | Upstream | Status | Reference impl |
|---|---|---|---|
| [ACP `session/suspend`](./acp-session-suspend.md) | agentclientprotocol/agent-client-protocol | Discussion open ([ACP #1233](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233)); no maintainer reply as of 2026-06-27 | `__host_worker_suspend` builtin + `_meta.harn.suspend`-decorated session updates; sibling to the already-shipped `session/resume` ([ACP #1726](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1726)) |
| [A2A `TASK_STATE_PAUSED`](./a2a-paused-state.md) | a2aproject/A2A | Discussion open ([A2A #1858](https://github.com/a2aproject/A2A/discussions/1858)); community feedback favors one `PAUSED` state plus structured `pause` metadata; no maintainer/TSC reply as of 2026-07-03; revised to A2A v1.0 conventions 2026-07-03 | `metadata.harn.pause`-decorated task status events |

[1848]: https://github.com/burin-labs/harn/issues/1848

### Agent identity & provenance ([#3330][3330])

| RFC | Upstream | Status | Reference impl |
|---|---|---|---|
| [A2A actor-chain extension](./a2a-actor-chain-extension.md) | a2aproject/A2A | Issue open ([A2A #2028](https://github.com/a2aproject/A2A/issues/2028), filed 2026-07-03); anchored to [A2A #1937](https://github.com/a2aproject/A2A/issues/1937) + [A2A #153](https://github.com/a2aproject/A2A/issues/153) | `ActorChain` carried under `metadata.actor_chain` in the `harn-serve` A2A adapter |
| [MCP `authenticatedIdentity`](./mcp-authenticated-identity.md) | modelcontextprotocol/modelcontextprotocol | Pre-SEP discussion open ([MCP #3008](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3008), filed 2026-07-03); SEP progression is sponsor-gated | Host-side "connected as" registry tracked under [harn#3331](https://github.com/burin-labs/harn/issues/3331) |
| [MCP/OAuth actor-token positioning](./oauth-actor-chain-positioning.md) | oauth-wg / modelcontextprotocol/ext-auth | Feedback posted on [oauth-wg #73](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73#issuecomment-4878092226) (2026-07-03) | `ActorChain` (RFC 8693 `act` shape) in `harn-vm` |

[3330]: https://github.com/burin-labs/harn/issues/3330

## Scope

Authoring RFC documents in this repository is in scope for these PRs.
**Filing upstream discussions, issues, or PRs is the maintainer's
explicit action - not a step any Harn contributor should take without
the project owner asking for it.** See [#1829][1829] for the upstream
work that remains open.

[1829]: https://github.com/burin-labs/harn/issues/1829

## Related

- [Harn ACP/MCP extensions v1](../spec/harn-extensions/v1.md) - the
  authoritative list of Harn-owned `_meta` fields that ride alongside
  upstream protocol payloads today.
- [System reminders](../system-reminders.md) - the language-level
  reminder primitive the reminder-injection RFCs are proposing to
  standardize.
- [Transcript architecture](../transcript-architecture.md) - the
  underlying transcript event model that produces and consumes
  reminders.
- [`suspend_agent` / `resume_agent` builtins](../builtins.md) - the
  language-level cooperative suspend primitive the
  suspend/resume RFCs are proposing to standardize.
- [Filing status ledger](./status-ledger.md) - direct links and
  verification notes for the current upstream discussions, PRs, and
  issues.
