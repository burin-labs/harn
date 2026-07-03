# MCP RFC: `authenticatedIdentity` on `InitializeResult`

**Upstream repo:** [modelcontextprotocol/modelcontextprotocol][mcp]
**Status:** Draft (not yet filed upstream). Verified non-duplicate on
2026-07-03: [SEP-1299][sep-1299] is unrelated (server-side OAuth flow
management, closed 2025-09-02) and [discussion #1827][mcp-1827]
(`upstream_identity`) runs the opposite direction (client→server
propagation). No SEP or discussion claims a server→client identity
surface. Filing path is a core SEP (Standards Track, sponsor-gated).
**Authors:** Burin Labs
**Reference impl:** MCP authenticated-identity registry and
"connected as" display tracked under
[harn#3331](https://github.com/burin-labs/harn/issues/3331) (Epic F);
per-server `whoami`-style probing ships in the Burin Code connector
surface today.

[mcp]: https://github.com/modelcontextprotocol/modelcontextprotocol
[sep-1299]: https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1299
[mcp-1827]: https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/1827

## Problem statement

After an MCP client completes an OAuth flow (or connects with any
credential), it has no standard way to learn *who the server thinks it
is connected as*. The identity exists — the server resolved it to
authorize the session — but the protocol never hands it back.

Every client that wants to render "Connected to Notion as
alice@example.com" reverse-engineers a per-server answer:

- call `notion-get-self` on Notion, `get_me` on GitHub-shaped servers,
  a `whoami` tool where one exists;
- parse a different response shape per server;
- give up and render "Connected" with no identity on servers that
  expose none.

That hurts in predictable ways:

- **Wrong-account connections go unnoticed.** OAuth flows complete
  against whichever account the browser session had live. Without a
  display identity, the user finds out when the agent writes to the
  wrong workspace.
- **Multi-account setups are unmanageable.** A host with several
  servers of the same type (work + personal) can't label them.
- **Audit trails start with a blank.** The host logs "session
  authorized" without a subject unless it invents per-server probing.

## Proposed shape

An optional field on `InitializeResult`:

```json
{
  "protocolVersion": "2025-11-25",
  "capabilities": { "...": "..." },
  "serverInfo": { "name": "example-server", "version": "1.2.0" },
  "authenticatedIdentity": {
    "subject": "alice@example.com",
    "displayName": "Alice Example",
    "provider": "example.com",
    "accountId": "usr_01HZY...",
    "_meta": {}
  }
}
```

- All fields optional except `subject`; the whole object is optional —
  servers that don't resolve an identity omit it.
- **Display and audit only, never authorization.** The field reports
  what the server already decided; clients MUST NOT treat it as a
  credential or derive permissions from it. (Same audit-not-authz rule
  as delegation chains: a reported identity is not a capability.)
- Identity can change mid-session (token refresh to a different
  account); a `notifications/identity_changed` companion is the natural
  follow-on but is deliberately out of the first proposal.

### Lighter alternative

A blessed output schema for a conventional `whoami` tool, so clients
can probe uniformly without an `initialize` change. This costs a
round-trip and a tool slot per server, and servers must opt in
tool-by-tool — but it needs no schema change. The RFC recommends the
`InitializeResult` field (identity is connection-scoped state, and
connect time is when the client renders the connection), with the
`whoami` schema documented as the fallback for hosts of older servers.

## Relationship to adjacent work

- [Discussion #1827][mcp-1827] (`upstream_identity`) is client→server:
  propagating the *caller's* identity into the server. This proposal is
  server→client: reporting the *authenticated session's* identity back.
  They compose; neither substitutes for the other.
- [SEP-1299][sep-1299] named the client-facing identity limitation in
  passing but was about server-side OAuth flow management and closed as
  redundant after URL elicitation. It did not claim this surface.
- The ext-auth enterprise-managed-authorization extension resolves
  identity server-side via ID-JAG; it makes this gap *more* visible,
  since the server provably knows the subject and still can't report
  it.

## Filing path

Per the [SEP guidelines][sep-guidelines]: pre-discussion, then a PR
adding `seps/0000-authenticated-identity.md`, with a named sponsor from
the maintainer list (required to leave Proposal state), a prototype
implementation before acceptance, and a conformance scenario before
Final. The reference-impl slot is Epic F (harn#3331): the host-side
registry ships regardless, keyed off `authenticatedIdentity` when
present and per-server `whoami` probing when not.

[sep-guidelines]: https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/community/sep-guidelines.mdx

## Open questions for upstream maintainers

1. **Field set.** Is `subject` + optional display fields the right
   core, or should this reuse an existing identity claim set (e.g. a
   subset of OIDC standard claims) verbatim?
2. **Placement.** `InitializeResult` (proposed) vs a dedicated
   `identity/get` request. Connect-time reporting favors the former;
   mid-session refresh favors the latter.
3. **Change notification.** Should `notifications/identity_changed`
   ship in v1, or wait for evidence that mid-session identity changes
   happen in practice?
4. **Privacy posture.** Should the spec require clients to display the
   identity only to the authenticated user's UI surface (not to the
   model context by default)?

## References

- [MCP discussion #1827 — `upstream_identity` (opposite direction)][mcp-1827]
- [SEP-1299 (unrelated; closed)][sep-1299]
- [SEP guidelines][sep-guidelines]
- [Sibling positioning note: MCP/OAuth actor-token](./oauth-actor-chain-positioning.md)
- [Epic F: authenticated-identity registry (harn#3331)](https://github.com/burin-labs/harn/issues/3331)
