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
3. **Open an upstream discussion** when the RFC is ready. Use the RFC
   doc as the source of truth for the discussion body so reviewers
   work from a single shared text. Do not expand scope inside the
   filed thread — drift looks bad and complicates maintainer triage.
4. **Track outcomes here.** When a proposal lands upstream, update the
   RFC's "Status" header and migrate the Harn `_meta` envelope to the
   standardized field locations in a follow-up PR. When a proposal is
   declined, record the decision in the RFC and keep the `_meta` shape
   stable for downstream consumers.

## Current RFCs

### Ambient reminder injection ([#1829][1829])

| RFC | Upstream | Status | Reference impl |
|---|---|---|---|
| [ACP `session/inject_reminder`](./acp-session-inject-reminder.md) | agentclientprotocol/agent-client-protocol | Discussion open ([ACP #1224](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224)) | `session/remind` JSON-RPC method + `_meta.harn.reminder`-decorated transcript events |
| [A2A `Message.kind: "Reminder"`](./a2a-message-kind-reminder.md) | a2aproject/A2A | Draft (not yet filed upstream) | `_meta.harn.reminder` on outbound A2A task messages |
| [MCP `notifications/reminder`](./mcp-notifications-reminder.md) | modelcontextprotocol/specification | Draft (not yet filed upstream) | `_meta.harn.reminder` on MCP server-emitted notifications |

### Suspend / resume + paused state ([#1848][1848])

| RFC | Upstream | Status | Reference impl |
|---|---|---|---|
| [ACP `session/suspend`](./acp-session-suspend.md) | agentclientprotocol/agent-client-protocol | Draft (not yet filed upstream) | `__host_worker_suspend` builtin + `_meta.harn.suspend`-decorated session updates; sibling to the already-shipped `session/resume` ([ACP #1726](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1726)) |
| [A2A `TaskState.PAUSED`](./a2a-paused-state.md) | a2aproject/A2A | Draft (not yet filed upstream) | `metadata.harn.pause`-decorated `tasks/statusUpdate` SSE events |

[1848]: https://github.com/burin-labs/harn/issues/1848

## Scope

Authoring RFC documents in this repository is in scope for these PRs.
**Filing upstream discussions, issues, or PRs is the maintainer's
explicit action — not a step any Harn contributor should take without
the project owner asking for it.** See [#1829][1829] for the upstream
work that remains open.

[1829]: https://github.com/burin-labs/harn/issues/1829

## Related

- [Harn ACP/MCP extensions v1](../spec/harn-extensions/v1.md) — the
  authoritative list of Harn-owned `_meta` fields that ride alongside
  upstream protocol payloads today.
- [System reminders](../system-reminders.md) — the language-level
  reminder primitive the reminder-injection RFCs are proposing to
  standardize.
- [Transcript architecture](../transcript-architecture.md) — the
  underlying transcript event model that produces and consumes
  reminders.
- [`suspend_agent` / `resume_agent` builtins](../builtins.md) — the
  language-level cooperative suspend primitive the
  suspend/resume RFCs are proposing to standardize.
