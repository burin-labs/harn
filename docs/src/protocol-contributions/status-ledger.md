# Protocol filing status ledger

Last verified: 2026-06-27 UTC.

This ledger records direct upstream reads for the protocol-contribution
threads that back the RFCs in this directory. It is intentionally factual:
only record a state here after fetching the upstream discussion, PR, or
issue directly.

In [Diataxis](https://diataxis.fr/) terms, this is reference material.
It lists current facts and links; it is not a how-to guide or a design
explanation.

## Public follow-up posture

Upstream comments and PRs should use neutral ecosystem evidence, peer
behavior, and public prior art. Do not lead with Harn or Burin-specific
adoption claims. A local implementation can inform our internal
confidence, but public posts should stand on protocol semantics and
independently checkable examples.

## ACP

| Item | Verified state | Notes |
|---|---|---|
| [`agentclientprotocol/agent-client-protocol#1220`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1220) | Open discussion. | A maintainer invited an RFD. The follow-up RFD is PR [`#1261`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261). |
| [`agentclientprotocol/agent-client-protocol#1261`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261) | Open PR, review required, mergeable as of 2026-06-27 after rebasing onto current `main`. | Rebase note posted at [`discussioncomment-4819339360`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261#issuecomment-4819339360). |
| [`agentclientprotocol/agent-client-protocol#484`](https://github.com/agentclientprotocol/agent-client-protocol/pull/484) | Closed in favor of `#1261`. | Relevant predecessor for prompt queueing / steer-via-yield framing. |
| [`agentclientprotocol/agent-client-protocol#1224`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224) | Open discussion, no maintainer response visible as of 2026-06-27. | Ambient system-role context injection / reminder sibling of `#1220`; next-step ping posted at [`discussioncomment-17455653`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224#discussioncomment-17455653). |
| [`agentclientprotocol/agent-client-protocol#1233`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233) | Open discussion, no maintainer response visible as of 2026-06-27. | `session/suspend` + `session/await_resumption`; next-step ping posted at [`discussioncomment-17455656`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233#discussioncomment-17455656). |
| [`agentclientprotocol/registry#397`](https://github.com/agentclientprotocol/registry/pull/397) | Open PR, mergeable, no checks or review decision as of 2026-06-27. | Registry submission; bump comment posted at [`issuecomment-4819339353`](https://github.com/agentclientprotocol/registry/pull/397#issuecomment-4819339353). |

## A2A

| Item | Verified state | Notes |
|---|---|---|
| [`a2aproject/A2A#1857`](https://github.com/a2aproject/A2A/discussions/1857) | Open discussion. | Separate idempotency / post-cancel semantics thread; not a pause/reminder substitute. |
| [`a2aproject/A2A#1858`](https://github.com/a2aproject/A2A/discussions/1858) | Open discussion, no maintainer/TSC reply visible as of 2026-06-27. | Good-faith community feedback converged toward one `PAUSED` state plus a structured `pause` object; draft-PR vs extension next-step ping posted at [`discussioncomment-17455657`](https://github.com/a2aproject/A2A/discussions/1858#discussioncomment-17455657). |

## MCP and OAuth Identity

| Item | Verified state | Notes |
|---|---|---|
| [`modelcontextprotocol/modelcontextprotocol#2736`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736) | Open discussion, no maintainer response as of 2026-06-27. | Per-call budget caps for `sampling/createMessage`; SEP-path next-step ping posted with AI disclosure at [`discussioncomment-17455659`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736#discussioncomment-17455659). |
| [`modelcontextprotocol/modelcontextprotocol#214`](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/214) | Closed. | Maintainer guidance on 2026-01-16 pointed custom auth pieces toward [`modelcontextprotocol/ext-auth`](https://github.com/modelcontextprotocol/ext-auth). |
| [`modelcontextprotocol/ext-auth#13`](https://github.com/modelcontextprotocol/ext-auth/issues/13) | Open. | Maintainer response says Enterprise-Managed Authorization does not currently support distinguishing agent vs user identity and points to ID-JAG issue `#73`. |
| [`oauth-wg/oauth-identity-assertion-authz-grant#73`](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73) | Open. | Proposal for workload / agent identity SSO and explicit delegated on-behalf-of access. |
| [`oauth-wg/oauth-identity-assertion-authz-grant#80`](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/80) | Closed, no visible comments. | Optional `actor_token` proposal split out from `#73`; no acceptance decision was visible in the fetched issue. |

## Local follow-up candidates

- Refresh ACP PR `#1261` against upstream `main`, resolve conflicts,
  and keep the public framing about queue/steer anchored in existing
  editor/agent behavior rather than local adoption.
- Consider an A2A `#1858` follow-up or draft PR only after narrowing
  the field set to the single-state `PAUSED` + `pause` object that the
  thread already converged toward.
- File A2A reminder and MCP reminder proposals only from neutral
  ambient-context examples such as editor file-watchers, build/test
  watchers, CI/review notifications, and existing host-hook/rules
  systems.
- For MCP identity, follow `ext-auth#13` / ID-JAG `#73` rather than
  reopening the closed MCP `#214`; keep any public comment focused on
  the protocol distinction between OAuth client continuity and runtime
  actor identity.
