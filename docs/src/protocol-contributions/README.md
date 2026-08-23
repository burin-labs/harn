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

Every status below was verified against the upstream thread on 2026-08-22. The
[filing status ledger](./status-ledger.md) carries the per-thread reading notes
behind them.

Across every open filing the pattern is the same: independent implementers
engage, and no maintainer has issued a verdict either way. Nothing has been
rejected. One proposal died for a different reason — see
[sampling budget caps](#by-protocol).

### Ambient reminder injection ([#1829][1829])

| RFC | Thread | Status | Reference impl |
|---|---|---|---|
| [ACP `session/inject_reminder`](./acp-session-inject-reminder.md) | [ACP #1224][acp-1224] | Open. Maintainer soft-parked it behind v2 draft triage on 2026-07-06 and asked for time. | `session/remind` JSON-RPC method plus `_meta.harn.reminder` on transcript events |
| [A2A `InjectTaskReminder`](./a2a-message-kind-reminder.md) | [A2A #2027][a2a-2027] | Open, cold. Filed 2026-07-03, still no comments. | `metadata.harn.reminder` on outbound A2A task events |
| [MCP `notifications/reminder`](./mcp-notifications-reminder.md) | [MCP #3007][mcp-3007] | Open, no comments. At risk: [SEP-2577][sep-2577] deprecated the adjacent Logging surface. | `_meta.harn.reminder` on MCP server-emitted notifications |

### Suspend, resume, and paused state ([#1848][1848])

| RFC | Thread | Status | Reference impl |
|---|---|---|---|
| [ACP `session/suspend`](./acp-session-suspend.md) | [ACP #1233][acp-1233] | Open, no maintainer reply. A peer implementer asked for an acknowledged request with a `requested → quiescing → suspended` state machine; we adopted that shape on 2026-07-25. | `__host_worker_suspend` builtin plus `_meta.harn.suspend` on session updates. Companion to the shipped `session/resume` ([ACP #1726][acp-1726]). |
| [A2A `TASK_STATE_PAUSED`](./a2a-paused-state.md) | [A2A #1858][a2a-1858] | Open, no maintainer or TSC reply. Community converged on one `PAUSED` state plus structured `pause` metadata; the open question is whether that warrants a draft PR. | `metadata.harn.pause` on task status events |

[1848]: https://github.com/burin-labs/harn/issues/1848

### Agent identity and provenance ([#3330][3330])

| RFC | Thread | Status | Reference impl |
|---|---|---|---|
| [A2A actor-chain extension](./a2a-actor-chain-extension.md) | [A2A #2028][a2a-2028] | Open and the most active filing we have, at 24 comments. The thread separated well-formedness from proven authority, then established that per-hop `scopes` are derived from a validated credential rather than authorization-bearing. Two participants agree on a shared vector set, but no maintainer has ever replied — see the [ledger caveat](./status-ledger.md#who-is-in-2028). | `ActorChain` under `metadata.actor_chain` in the `harn-serve` A2A adapter |
| [MCP `authenticatedIdentity`](./mcp-authenticated-identity.md) | [MCP #3008][mcp-3008] | Open. One implementer confirmed the gap on 2026-07-12; no maintainer reply. Needs a sponsor, because SEP progression is sponsor-gated. | Host-side "connected as" registry, tracked under [harn#3331](https://github.com/burin-labs/harn/issues/3331) |
| [MCP/OAuth actor-token positioning](./oauth-actor-chain-positioning.md) | [oauth-wg #73][oauth-73] | Open, and the ball is with us. A draft author pointed the actor-chain conversation at [`draft-mcguinness-oauth-actor-profile`][actor-profile] on 2026-07-25; that has not been answered. | `ActorChain` (RFC 8693 `act` shape) in `harn-vm` |

[3330]: https://github.com/burin-labs/harn/issues/3330

### By protocol

The same documents, grouped by the protocol they target:

| Protocol | Documents |
|---|---|
| **ACP** | [`session/inject_reminder`](./acp-session-inject-reminder.md) · [`session/suspend`](./acp-session-suspend.md) · [typed host-event injection](./acp-session-inject-host-event.md) (shipped extension, not filed upstream) |
| **A2A** | [`InjectTaskReminder`](./a2a-message-kind-reminder.md) · [`TASK_STATE_PAUSED`](./a2a-paused-state.md) · [actor-chain extension](./a2a-actor-chain-extension.md) |
| **MCP** | [`notifications/reminder`](./mcp-notifications-reminder.md) · [`authenticatedIdentity`](./mcp-authenticated-identity.md) · [sampling budget caps](./mcp-sampling-budget-caps.md) (closed — [SEP-2577][sep-2577] deprecated the surface it extended) |
| **OAuth** | [actor-token positioning](./oauth-actor-chain-positioning.md) (implementer feedback, no new proposal) |

[acp-1224]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224
[acp-1233]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233
[acp-1726]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1726
[a2a-1858]: https://github.com/a2aproject/A2A/discussions/1858
[a2a-2027]: https://github.com/a2aproject/A2A/discussions/2027
[a2a-2028]: https://github.com/a2aproject/A2A/issues/2028
[mcp-3007]: https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3007
[mcp-3008]: https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3008
[sep-2577]: https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2577
[oauth-73]: https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73
[actor-profile]: https://github.com/mcguinness/draft-mcguinness-oauth-actor-profile

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
