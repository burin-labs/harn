# ADR 0003: hand-rolled MCP vs. the official `rmcp` SDK

## Status

Accepted. **Decision: keep the hand-rolled MCP implementation; do not adopt
`rmcp` as a dependency.** A viability spike (summarised below) confirmed `rmcp`
*could* host every Harn-specific protocol feature without a fork, so this is a
deliberate "viable but not compelling" call rather than a "can't be done" one.

The companion DRY work that motivated the wider review — routing the
`harn-cloud-gateway` remote-attach handlers through the shared
`harn_vm::jsonrpc` envelope builders — shipped separately and is unaffected by
this decision.

## Context

Harn ships a complete, hand-rolled MCP (Model Context Protocol) client and
server, plus an in-progress `DRAFT-2026-v1` revision of the protocol that Harn
is proposing upstream. The recurring question is whether this is "reinventing
the wheel" now that an official Rust SDK — [`rmcp`](https://crates.io/crates/rmcp)
(currently 1.7.0) — exists and is maintained by the protocol authors.

The pull toward `rmcp` is real: an official SDK tracks the spec, ships security
patches, and would let us delete protocol plumbing we currently own. The
question is whether that pull survives contact with (a) what `rmcp` actually
covers, (b) what Harn's MCP surface actually *is*, and (c) Harn's strategic
position as a party that proposes spec revisions rather than only consuming
them.

### What Harn's MCP surface actually is

A survey of the codebase (2026-05-30) put the MCP-specific footprint at
**~24k LOC across four crates**:

| Area | Crate / module | ~LOC | rmcp-replaceable? |
|---|---|---|---|
| Protocol & version negotiation | `harn-vm/src/mcp_protocol.rs` | 0.8k | yes |
| Client (stdio + HTTP/SSE) | `harn-vm/src/mcp.rs` | 2.8k | yes |
| Transport negotiation | `harn-serve/src/adapters/mcp/transport.rs` | 0.5k | yes |
| Server dispatch + handlers | `harn-vm/src/mcp_server/*` | 2.8k | partial |
| Capability builders | `harn-vm/src/mcp_protocol.rs` + servers | ~1.5k | partial |
| `VmValue` ↔ JSON bridge | `harn-vm/src/mcp_server/convert.rs` | 0.2k | **no** |
| OAuth 2.0 PKCE / token refresh | `harn-vm/src/mcp_auth.rs` | 1.4k | **no** |
| Elicitation (server→client) | `harn-vm/src/mcp_elicit.rs` | 0.7k | **no** |
| Sampling (server→client) | `harn-vm/src/mcp_sampling.rs` | 1.1k | **no** |
| Progress notifications | `harn-vm/src/mcp_progress.rs` | 0.4k | **no** |
| File upload (SEP-2356) | `harn-vm/src/mcp_file_upload.rs` | 0.9k | **no** |
| Capability allowlisting / host bridge | `harn-vm/src/mcp_host.rs`, `mcp_allowlist.rs` | 1.7k | **no** |
| Wire-conformance harness | `harn-mcp-rc-compat/*` | 2.0k | n/a |

The replaceable surface (JSON-RPC envelopes, transport, version negotiation) is
~9.5k LOC — and it is the **cheap, stable** part. The ~12k LOC that `rmcp` does
*not* cover (auth, elicitation, sampling, progress, file upload, the capability
allowlist, and the `VmValue` bridge) is the **expensive, Harn-specific** part
that would stay hand-rolled under any adoption path.

### What `DRAFT-2026-v1` is

`DRAFT-2026-v1` is **Harn's own proposed protocol revision**, not a released
spec. Its additive surface (in `crates/harn-vm/src/mcp_protocol.rs`) includes:

- `server/discover` — stateless entry point (no `initialize` round-trip),
- `completion/complete` — parameterised completion with ranking/dedupe,
- the `tasks/*` family (`get` / `result` / `list` / `cancel`) with
  `_meta.progressToken` progress,
- `elicitation/create` and `sampling/createMessage` (server→client),
- per-request protocol negotiation via the `io.modelcontextprotocol/*` `_meta`
  keys and the `mcp-protocol-version` HTTP header,
- the `resultType: "complete" | "input_required"` response discriminant,
- `-32004` "unsupported protocol version" error code.

These are exactly the methods Harn cares most about, and they are the ones
`rmcp` 1.7 does **not** model as typed requests (it predates the revision).

## Survey / spike

Before deciding, we built a standalone spike (`/tmp/rmcp-spike`, `rmcp = "1"`
with `server,client,transport-async-rw`) to falsify the strongest pro-adoption
claim: *"`rmcp` can't carry `DRAFT-2026-v1`, so adopting it would regress Harn's
RC."* Six tests, all green:

| Mechanism Harn hand-rolls | rmcp escape hatch | Result |
|---|---|---|
| Unknown method (`server/discover`, `completion/complete`, …) | `ClientRequest::CustomRequest` (untagged catch-all, last variant) → `ServerHandler::on_custom_request` | routes correctly |
| Typed method (`initialize`) not hijacked by the catch-all | const-string variant matched before the catch-all | not hijacked |
| Arbitrary protocol-version string (`DRAFT-2026-v1`) | `ProtocolVersion` is a string newtype | round-trips |
| Experimental capability flags (`io.harn/draft-2026`) | `ServerCapabilities.experimental` / `ClientCapabilities.experimental` maps | advertise cleanly |
| `_meta` sidecar (per-request negotiation, cache hints) | hoisted into a typed `Meta` extension on deserialize, **re-merged into `params` on serialize** | preserved, *better* ergonomics than raw params |
| End-to-end custom dispatch over a real transport | `serve_server` + `serve_client` over `tokio::io::duplex`; `send_request(CustomRequest)` → `ServerResult::CustomResult` | dispatches end-to-end |

The dispatch path is confirmed at the source level too: rmcp's server handler
routes `ClientRequest::CustomRequest(request) => self.on_custom_request(...)`
(`rmcp-1.7.0/src/handler/server.rs:110`), and rmcp's own test-suite asserts that
an unknown method deserialises to `CustomRequest` (`.../src/model.rs:3524`).

**Conclusion of the spike: adoption is viable.** `rmcp` can host all of
`DRAFT-2026-v1` without forking the crate. The "switching would regress the RC"
objection is false.

(Two spike gotchas worth recording for anyone re-running it: `rmcp`'s
capability/info structs are `#[non_exhaustive]`, so build them by mutating a
`default()` rather than with a struct literal; and the in-process
`duplex` handshake deadlocks unless `serve_server` and `serve_client` are driven
concurrently with `tokio::join!`, because the initialize exchange is mutual.)

## Decision

**Keep the hand-rolled implementation. Do not take an `rmcp` dependency in
`harn-vm` (or any published Harn crate).** Viability is necessary but not
sufficient; adoption is not *compelling*, for three reasons:

1. **The replaceable surface is the cheap surface.** `rmcp` would absorb
   ~9.5k LOC of JSON-RPC envelopes, transport, and version negotiation — the
   most stable, least-churning code we own. The ~12k LOC that actually carries
   Harn's value (OAuth, elicitation, sampling, progress, file upload, the
   capability allowlist, the `VmValue` bridge) is **not** covered by `rmcp` and
   stays hand-rolled regardless. The maintenance dividend is modest.

2. **Our own RC would live permanently in an escape hatch.** Because `rmcp`
   does not model `DRAFT-2026-v1`, every RC method (`server/discover`,
   `completion/complete`, `tasks/*`, the `resultType` discriminant, …) would be
   dispatched as a `CustomRequest` through `on_custom_request`. The dispatch
   `match` does not shrink — it *moves* into the escape hatch and loses the
   typed-variant ergonomics for exactly the methods we care about most. We would
   be a **downstream follower** of the official SDK's release cadence for
   features we are trying to **lead** upstream. Hand-rolling is what lets Harn
   ship the revision ahead of the SDK and propose it from a position of a
   working implementation.

3. **Migration risk outweighs the savings.** Adoption means rewriting working,
   tested code; injecting `rmcp`'s transport/async dependency tree into
   `harn-vm`, which is consumed as a **published crate** (`harn-vm = "=0.8.x"`)
   by `harn-cloud` and others, so every dependent inherits the tree; and risking
   wire-level drift that the dedicated `harn-mcp-rc-compat` conformance harness
   exists specifically to prevent. The existence of that harness is itself a
   signal that the project values byte-level control `rmcp` would abstract away.

This is consistent with Harn's broader positioning: own the primitives that are
strategic moats, lean on vendors for commodity surface. MCP wire framing, *for a
party proposing spec revisions*, is strategic, not commodity.

### Preserve the evidence, not the dependency

The spike is **not** committed into the repo. Committing an `rmcp`-backed test
would inject the very dependency tree this ADR declines, contradicting its own
rationale. Instead, this ADR records the spike's methodology and results so the
decision is reproducible (the spike lived at `/tmp/rmcp-spike`; re-creatable from
the test table above in well under an hour). The wire format remains guarded by
the existing `harn-mcp-rc-compat` harness, which tests Harn's bytes directly
without depending on any external SDK.

## Consequences

### Positive

- **No new heavy dependency** enters `harn-vm` or any published crate; CI cost
  and the dependent-crate blast radius are unchanged.
- **The RC stays first-class.** `DRAFT-2026-v1` methods remain typed,
  directly dispatched, and shippable ahead of the official SDK — preserving
  Harn's ability to propose the revision from a working implementation.
- **Wire control is retained**, guarded by `harn-mcp-rc-compat` rather than
  delegated to an SDK whose abstractions we'd have to reverse-engineer to debug.
- **The "are we reinventing the wheel?" question is now answered with
  evidence**, not vibes: yes for the commodity envelope layer (and we DRY'd the
  worst offender via `harn_vm::jsonrpc`), no for the protocol/RC layer.

### Negative / risks

- **We continue to own ~9.5k LOC of commodity protocol plumbing** and its
  maintenance, including tracking upstream MCP spec changes by hand. Mitigation:
  the `harn-mcp-rc-compat` harness and the periodic upstream-protocol review
  already in place.
- **No automatic spec-conformance from an official SDK.** If the official spec
  diverges from our framing in ways our harness does not catch, we find out
  later than an `rmcp` consumer would. Mitigation: keep the spike methodology on
  file (this ADR) and re-run it against new `rmcp` releases as a cheap external
  conformance oracle — *using* `rmcp` to check our bytes without *depending* on
  it in the shipping crates.

### Deferred / revisit triggers

This decision should be **revisited** if any of the following change:

- **`rmcp` ships `DRAFT-2026-v1` (or the official equivalent) as typed
  requests.** At that point the RC methods would no longer be escape-hatch-only,
  and reason (2) above weakens substantially — adoption for the *client* side in
  particular (`harn-vm/src/mcp.rs`, where Harn is purely a *consumer* of
  third-party servers and has no strategic reason to hand-roll) becomes worth a
  fresh, bounded spike.
- **The hand-rolled transport/client accrues a class of bug (reconnect, SSE
  edge cases, HTTP/2) that `rmcp` demonstrably handles** — a maintenance-cost
  signal that would tip reason (1).
- **Harn stops proposing protocol revisions** and becomes a pure consumer, which
  would dissolve reason (2) entirely.

A client-only `rmcp` adoption spike is the natural next probe if any trigger
fires; it is explicitly **not** in scope now.

## Verification

- The viability spike: six tests green (table above), including the end-to-end
  `on_custom_request` dispatch over a real transport.
- Source-level dispatch confirmation in `rmcp-1.7.0`:
  `src/handler/server.rs:110` (routing) and `src/model.rs:3524` (catch-all
  deserialisation), independent of the in-process test plumbing.
- Surface inventory: the four-crate, ~24k-LOC map in the Context section,
  cross-checked against `Cargo.toml`/`Cargo.lock` (confirmed: **zero** existing
  `rmcp` dependency anywhere in the workspace).
- The companion DRY cutover (`harn-cloud-gateway` → `harn_vm::jsonrpc`) compiles
  clean (`cargo check -p harn-cloud-gateway`) with byte-identical wire output.
