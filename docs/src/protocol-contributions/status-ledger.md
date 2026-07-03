# Protocol filing status ledger

Last verified: 2026-07-03 UTC.

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

Context as of 2026-07-03: the v2 RFD collection was marked "Active" on
2026-07-02 and is the most active area of the repo, but every v2 RFD PR
merged since 2026-06-01 was maintainer-authored. Externally-authored RFD
PRs (including `#1261`) sit in review while their content gets absorbed
into `unstable-v2` commits. Plan for discussions to be the unit of
influence, not PR merges. Two v2 docs matter for our filings: the merged
session-resume-replay RFD (unified `session/load` + `session/resume`,
optional `replayFrom` cursor) is the substrate any `session/suspend`
follow-up must sit on, and open PR
[`#1237`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1237)
(client-provided system prompt) overlaps the `session/remind` territory
and should be differentiated against, not ignored.

| Item | Verified state | Notes |
|---|---|---|
| [`agentclientprotocol/agent-client-protocol#1220`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1220) | Open discussion. | A maintainer invited an RFD. The follow-up RFD is PR [`#1261`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261). |
| [`agentclientprotocol/agent-client-protocol#1261`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261) | Open PR, mergeable, review still required as of 2026-07-03; no maintainer review since the 2026-06-27 rebase. | Rebase note posted at [`discussioncomment-4819339360`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261#issuecomment-4819339360). |
| [`agentclientprotocol/agent-client-protocol#484`](https://github.com/agentclientprotocol/agent-client-protocol/pull/484) | Closed in favor of `#1261`. | Relevant predecessor for prompt queueing / steer-via-yield framing. |
| [`agentclientprotocol/agent-client-protocol#1224`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224) | Open discussion, no maintainer response visible as of 2026-07-03. | Ambient system-role context injection / reminder sibling of `#1220`; the 2026-06-27 next-step ping ([`discussioncomment-17455653`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224#discussioncomment-17455653)) is still unanswered. |
| [`agentclientprotocol/agent-client-protocol#1233`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233) | Open discussion, no maintainer response visible as of 2026-07-03. | `session/suspend` + `session/await_resumption`; the 2026-06-27 next-step ping ([`discussioncomment-17455656`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233#discussioncomment-17455656)) is still unanswered. |
| [`agentclientprotocol/registry#397`](https://github.com/agentclientprotocol/registry/pull/397) | Open PR, no review decision as of 2026-07-03. | Registry submission; entry refreshed to Harn v0.8.158 with revalidated archive URLs on 2026-06-30. |

## A2A

Context as of 2026-07-03: A2A v1.0.0 shipped 2026-03-12 (v1.0.1 on
2026-05-28), which predates our 2026-05-17 filings. v1.0 renamed
operations to PascalCase across all bindings (`message/send` →
`SendMessage`, subscription → `SubscribeToTask`), moved enums to
`SCREAMING_SNAKE_CASE` per ProtoJSON, removed `kind` discriminators,
and made `a2a.proto` the normative source of truth. Both filed
discussions and the RFCs in this directory originally used pre-1.0
naming; the RFCs were revised to v1.0 conventions on 2026-07-03, and
any follow-up upstream post should use v1.0 names.

| Item | Verified state | Notes |
|---|---|---|
| [`a2aproject/A2A#1857`](https://github.com/a2aproject/A2A/discussions/1857) | Open discussion. | Separate idempotency / post-cancel semantics thread; not a pause/reminder substitute. |
| [`a2aproject/A2A#1858`](https://github.com/a2aproject/A2A/discussions/1858) | Open discussion, no maintainer/TSC reply visible as of 2026-07-03. | Good-faith community feedback converged toward one `PAUSED` state plus a structured `pause` object; the draft-PR-vs-extension next-step ping ([`discussioncomment-17455657`](https://github.com/a2aproject/A2A/discussions/1858#discussioncomment-17455657)) is still unanswered. |
| [`a2aproject/A2A#1937`](https://github.com/a2aproject/A2A/issues/1937) | Open issue, last updated 2026-06-19. | Context-binding profile for delegated authority — binds an already-valid delegation to a task/session/target/scope. Complement of (not substitute for) the [actor-chain extension RFC](./a2a-actor-chain-extension.md); best anchor thread for that filing. |
| [`a2aproject/A2A#153`](https://github.com/a2aproject/A2A/issues/153) | Open issue (since 2025-06). | Confused-deputy framing for A2A; canonical motivation citation for payload-visible principals. |

## MCP and OAuth Identity

| Item | Verified state | Notes |
|---|---|---|
| [`modelcontextprotocol/modelcontextprotocol#2736`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736) | Open discussion, no maintainer response as of 2026-07-03; first substantive community reply landed 2026-06-27 after the SEP-path ping ([`discussioncomment-17455659`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736#discussioncomment-17455659)). | The reply endorses a deliberately small first SEP: one host-owned limit envelope plus one typed stop/failure shape, with a decision-basis receipt (estimated vs actual usage, applied policy limit, meter basis, preflight-vs-mid-call-vs-post-hoc stop cause) and append-only follow-up records for retries/overrides. |
| [`modelcontextprotocol/modelcontextprotocol#214`](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/214) | Closed. | Maintainer guidance on 2026-01-16 pointed custom auth pieces toward [`modelcontextprotocol/ext-auth`](https://github.com/modelcontextprotocol/ext-auth). |
| [`modelcontextprotocol/ext-auth#13`](https://github.com/modelcontextprotocol/ext-auth/issues/13) | Open, no activity since 2026-01-31. | Maintainer response says Enterprise-Managed Authorization does not currently support distinguishing agent vs user identity and points to ID-JAG issue `#73`. |
| [`oauth-wg/oauth-identity-assertion-authz-grant#73`](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73) | Open, last updated 2026-04-02. | The live venue for actor-chain work: workload / agent identity SSO and explicit delegated on-behalf-of access, reusing RFC 8693 `act`. See the [positioning note](./oauth-actor-chain-positioning.md). |
| [`oauth-wg/oauth-identity-assertion-authz-grant#80`](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/80) | Closed as completed and milestoned 2026-04-22. | Optional `actor_token` proposal split out from `#73`; folded into the `#73` direction rather than rejected. |
| [`modelcontextprotocol/modelcontextprotocol#1299`](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1299) | Closed as completed 2025-09-02. | SEP-1299 is server-side OAuth flow management, unrelated to a server→client identity surface; it does not claim the `authenticatedIdentity` slot. |
| [`modelcontextprotocol/modelcontextprotocol` discussion `#1827`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/1827) | Open discussion, unanswered (opened 2025-11-17). | `upstream_identity` propagation, client→server — the opposite direction from the [`authenticatedIdentity` RFC](./mcp-authenticated-identity.md); the two compose. |

## Local follow-up candidates

- Keep ACP PR `#1261` conflict-free against upstream `main` while it
  waits on maintainer review, and keep the public framing about
  queue/steer anchored in existing editor/agent behavior rather than
  local adoption.
- A2A `#1858`: the RFC in this directory now matches the converged
  single-state `PAUSED` + `pause` object. A draft PR is ready to cut
  if maintainers answer the pending draft-PR-vs-extension question.
- MCP `#2736`: the 2026-06-27 community reply endorses a small first
  SEP (host-owned limit envelope + typed stop/failure + decision-basis
  receipt). A follow-up reply narrowing the proposal to that scope is
  the next external step.
- File A2A reminder and MCP reminder proposals only from neutral
  ambient-context examples such as editor file-watchers, build/test
  watchers, CI/review notifications, and existing host-hook/rules
  systems.
- For MCP identity, follow `ext-auth#13` / ID-JAG `#73` rather than
  reopening the closed MCP `#214`; keep any public comment focused on
  the protocol distinction between OAuth client continuity and runtime
  actor identity.
