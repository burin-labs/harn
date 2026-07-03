# Positioning note: actor chains in MCP enterprise auth (ID-JAG)

**Status:** Feedback posted 2026-07-03 on
[oauth-wg #73](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73#issuecomment-4878092226).
Verified 2026-07-03. Unlike
the other documents in this directory, there is no new proposal to
file: the substance already lives in active upstream threads, and the
right contribution is implementer feedback there.
**Authors:** Burin Labs

## The gap

MCP's enterprise-managed-authorization extension (the one **Stable**
extension in [modelcontextprotocol/ext-auth][ext-auth-spec]) obtains
tokens via ID-JAG (RFC 8693 token exchange), but carries a single
delegated subject — no `actor_token`, so a nested-agent hop chain
(user → orchestrator agent → sub-agent → MCP server) collapses to one
subject and the intermediate actors vanish from the token.

## Where the conversation actually is

- [ext-auth #13][ext-auth-13] — "Clarifying Agent vs User Identity"
  (open, stalled since 2026-01-31). The maintainer response confirms
  enterprise-managed-auth does not distinguish agent vs user identity
  and points at ID-JAG for the fix.
- [oauth-wg/oauth-identity-assertion-authz-grant #73][idjag-73] — the
  live thread (open, updated 2026-04-02): workload/agent identity SSO
  and explicit delegated on-behalf-of access, reusing RFC 8693 `act`,
  explicitly enabling future actor chains.
- [oauth-wg #80][idjag-80] — optional `actor_token` for ID-JAG; closed
  completed and milestoned 2026-04-22, i.e. folded into the #73
  direction rather than rejected.
- [MCP #214](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/214)
  — closed; maintainers pointed custom auth work at ext-auth. Do not
  reopen.

## Our posture

Adopt-not-invent (the Identity program's standing invariant): the
internal `ActorChain` representation
([`crates/harn-vm/src/actor_chain.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-vm/src/actor_chain.rs))
is already RFC 8693 `act`-shaped so that whatever ID-JAG ratifies is a
drop-in. **Do not file a new MCP SEP or discussion for actor chains** —
the venue is oauth-wg #73 (and ext-auth #13 for the MCP-specific
consequence).

What we can usefully contribute upstream, as implementer feedback on
[#73][idjag-73]:

1. A concrete nested-agent topology where the chain drops today
   (multi-hop agent delegation reaching an MCP server through
   enterprise-managed auth), and what the audit trail loses.
2. Support for keeping nested actors audit-only (authorization from
   top-level claims only) — the audit-not-authz split we enforce in the
   reference implementation.
3. A data point that scope attenuation per hop is worth representing:
   each hop in our chain carries the scopes it held, so narrowing is
   visible in the audit record.

Tracked in [harn#3345](https://github.com/burin-labs/harn/issues/3345)
(Identity E2). Any upstream comment requires maintainer sign-off per
the pro-bono filing pattern in [README.md](./README.md).

[ext-auth-spec]: https://github.com/modelcontextprotocol/ext-auth/blob/main/specification/stable/enterprise-managed-authorization.mdx
[ext-auth-13]: https://github.com/modelcontextprotocol/ext-auth/issues/13
[idjag-73]: https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73
[idjag-80]: https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/80
