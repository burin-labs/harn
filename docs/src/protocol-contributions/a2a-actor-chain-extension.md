# A2A RFC: actor-chain extension for delegated authority

**Upstream repo:** [a2aproject/A2A][a2a]
**Status:** Draft (not yet filed upstream). Verified non-duplicate on
2026-07-03; anchor threads are [A2A #1937][a2a-1937] (context-binding
profile for delegated authority) and [A2A #153][a2a-153] (confused
deputy). Written against A2A v1.0 conventions (`a2a.proto` normative,
PascalCase operations, ProtoJSON enums).
**Authors:** Burin Labs
**Reference impl:** `harn_vm::actor_chain::ActorChain`
([`crates/harn-vm/src/actor_chain.rs`][actor-chain-rs]) carried through
the `harn-serve` A2A adapter
([`crates/harn-serve/src/adapters/a2a/schema.rs`][a2a-schema-rs]) under
task/message `metadata` today.

[a2a]: https://github.com/a2aproject/A2A
[a2a-1937]: https://github.com/a2aproject/A2A/issues/1937
[a2a-153]: https://github.com/a2aproject/A2A/issues/153
[actor-chain-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-vm/src/actor_chain.rs
[a2a-schema-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-serve/src/adapters/a2a/schema.rs

## Problem statement

A2A v1.0 carries no identity in the payload. Authentication is
out-of-band: `SecurityScheme` declarations on the agent card plus
transport-level mechanisms (HTTP auth, mTLS). The canonical data model
has no field on `Message`, `Task`, or the agent card for the human (or
upstream agent) a request is made *on behalf of*. The only
identity-adjacent field is `tenant`, which is an opaque routing id, not
a principal. The only escape hatch is the untyped `metadata` map.

That gap matters as soon as agents delegate to agents:

- **Attribution.** When agent C receives work from agent B, which
  received it from agent A acting for user U, C sees only B's transport
  credentials. The full chain A→B→C-for-U exists nowhere on the wire.
- **Audit.** Post-hoc "who caused this side effect" reconstruction has
  to join transport logs across every hop, each with its own identity
  representation.
- **Confused deputy** ([#153][a2a-153]): a callee that can't see the
  originating principal can be tricked into using its own broader
  authority on behalf of a less-privileged caller.

The OAuth ecosystem already has the vocabulary for this: RFC 8693 token
exchange expresses delegation as nested `act` (actor) claims, and the
ID-JAG / WIMSE drafts under active development build agent-specific
delegation on the same shape. A2A can interoperate with that work
without inventing a token format — the missing piece is a standard,
optional place in the A2A payload where a chain travels.

[#1937][a2a-1937] proposes the complementary half: *binding* an
already-valid delegation to a task/session/target/scope. This proposal
is about *carrying* the chain itself in a standard shape. The two
compose: a context-binding profile needs a chain representation to
bind.

## Proposed shape

An A2A **extension** (v1.0 extension mechanism: an `AgentExtension`
entry in `AgentCard.capabilities.extensions[]`, keyed by URI), rather
than a core schema change. Peers that support it declare:

```json
{
  "capabilities": {
    "extensions": [
      {
        "uri": "https://a2a-protocol.org/extensions/actor-chain/v1",
        "required": false
      }
    ]
  }
}
```

The extension defines one reserved metadata member, `actorChain`, valid
on `Message.metadata` and echoed on `Task.metadata`:

```json
{
  "actorChain": {
    "origin": { "sub": "user:alice@example.com" },
    "actors": [
      { "sub": "agent:coordinator", "scopes": ["repo:read", "ci:trigger"] },
      { "sub": "agent:rebase-worker", "scopes": ["repo:read"] }
    ],
    "mayAct": "agent:rebase-worker"
  }
}
```

- `origin` — the outermost principal the work is ultimately for
  (typically a human).
- `actors` — the delegation hops in order, mirroring RFC 8693 nested
  `act` claims flattened for readability; each hop optionally carries
  the authority scopes that hop held, so attenuation is visible.
- `mayAct` — optional RFC 8693 `may_act` analogue.

**Audit, not authz.** The chain is attribution and audit history. It
MUST NOT be used to grant access: authorization stays with the
transport-level credentials and whatever token the callee validated.
This mirrors the position the ID-JAG authors have taken and avoids the
"self-asserted identity as capability" trap — a payload field anyone
can write must never be a credential.

## Compatibility and migration

The extension is additive and optional. Peers that don't declare it
ignore the metadata member (A2A metadata is already
tolerate-by-default). Peers that do declare it SHOULD echo the chain
(with their own hop appended) when they delegate onward.

The reference implementation ships this today under
`metadata.actor_chain` / `metadata.harn.actor_chain` with the same
entry shape (`sub` + `scopes`), validated on ingest and appended per
hop. Migration to a ratified extension URI and member name is a rename.

## Open questions for upstream maintainers

1. **Extension vs core.** Is the v1.0 extension mechanism the intended
   incubation path for this, or is payload identity in scope for core
   once [#1937][a2a-1937] settles?
2. **Nested vs flattened.** RFC 8693 nests `act` claims; the shape
   above flattens to an ordered array for readability. Should the wire
   shape stay byte-compatible with RFC 8693 claims instead?
3. **Relationship to signed evidence.** A2A v1.0 already does JWS for
   agent-card signatures. Should hops be signable, or is that
   deliberately deferred to token-level mechanisms (ID-JAG / WIMSE)?
4. **Interaction with `tenant`.** Should the extension state that
   `tenant` remains routing-only, to keep it from being repurposed as
   a principal?

## References

- [A2A #1937 — context-binding profile for delegated authority][a2a-1937]
- [A2A #153 — confused deputy][a2a-153]
- [RFC 8693 — OAuth 2.0 token exchange](https://www.rfc-editor.org/rfc/rfc8693)
- [oauth-wg ID-JAG #73 — workload/agent identity SSO and explicit
  delegated on-behalf-of access](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73)
- [Sibling positioning note: MCP/OAuth actor-token](./oauth-actor-chain-positioning.md)
- [`ActorChain` reference implementation][actor-chain-rs]
