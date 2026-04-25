# Agents Protocol stdlib gap audit (issue #634)

First-cut survey of `harn-vm` stdlib primitives needed before a Harn-native
Harness reference implementation (paired with the harn-cloud reference in
[#631](https://github.com/burin-labs/harn/issues/631)) can shrink its Rust
shell to "just the runtime adapter."

This document is **audit-only**. No stdlib expansion lands in the same
PR — every gap below is tracked in a dedicated sub-ticket so the
expansions can be reviewed, soaked, and rolled in independently.

Snapshot taken against `harn-vm 0.7.39` (commit
`2505c06d Add compression stdlib builtins (#630)`).

## Method

1. Enumerated every `vm.register_builtin` / `register_async_builtin`
   site under `crates/harn-vm/src/` (≈590 unique names).
2. For each gap candidate from the parent issue, determined whether
   `.harn` code can already accomplish it without dropping back into
   Rust.
3. Where the answer was "no," wrote a sub-ticket whose done-criteria
   include the authoritative-registry rule, the `make gen-highlight`
   coordination rule, and tests for the dangerous edges.
4. Where the answer was "yes (already covered)," recorded the existing
   builtin set so the Harness reference impl can rely on it without
   re-litigating.

The audit deliberately ignores host-side concerns owned by harn-cloud
(tenant policy, billing, receipt authorization). Per
[CLAUDE.md](../CLAUDE.md): Harn owns runtime primitives; hosts own
mutation/UX/policy.

## What already exists

### Outbound HTTP, SSE, WebSocket

| Need | Existing builtins |
| ---- | ----------------- |
| HTTP client | `http_get`, `http_post`, `http_put`, `http_patch`, `http_delete`, `http_request` |
| Pooled session | `http_session`, `http_session_request`, `http_session_close` |
| Mock/testkit | `http_mock`, `http_mock_clear`, `http_mock_calls`, `transport_mock_clear`, `transport_mock_calls` |
| SSE client | `sse_connect`, `sse_receive`, `sse_close`, `sse_mock` |
| WebSocket client | `websocket_connect`, `websocket_send`, `websocket_receive`, `websocket_close`, `websocket_mock` |

Source: [`crates/harn-vm/src/http.rs`](../crates/harn-vm/src/http.rs).

Outbound multipart bodies, streaming requests, and custom TLS for the
client are tracked separately in
[#616](https://github.com/burin-labs/harn/issues/616) (open;
PR [#629](https://github.com/burin-labs/harn/pull/629)) and are
treated as orthogonal to this audit.

### Crypto / encoding (covers the #634 list almost entirely)

| Need | Existing builtins | Notes |
| ---- | ----------------- | ----- |
| Base64 / base64url / base32 | `base64_encode`, `base64_decode`, `base64url_encode`, `base64url_decode`, `base32_encode`, `base32_decode`, `bytes_to_base64`, `bytes_from_base64` | base64url is the no-pad variant |
| Hex | `hex_encode`, `hex_decode`, `bytes_to_hex`, `bytes_from_hex` | |
| Hash families | `sha256`, `sha224`, `sha384`, `sha512`, `sha512_256`, `md5` | hex output |
| HMAC-SHA256 | `hmac_sha256`, `hmac_sha256_base64` | matches GitHub-style and Slack-style webhook signatures |
| Constant-time compare | `constant_time_eq` | mandatory for HMAC verify |
| URL encode / decode | `url_encode`, `url_decode` | |
| JWT (sign) | `jwt_sign` | ES256 + RS256 |
| Stable bucket hash | `hash_value`, `compute_content_hash` | non-crypto |
| Non-crypto random | `random`, `random_int` | f64 / int — see gap below for CSPRNG bytes |

Source: [`crates/harn-vm/src/stdlib/crypto.rs`](../crates/harn-vm/src/stdlib/crypto.rs).

Verdict: the #634 explicit list ("HMAC families, hex/base64url,
constant-time comparison") is already covered. Three nice-to-haves
remain — see "Crypto follow-ups" below.

### Webhook ingress (already pure-Rust)

`harn_serve` (the outbound workflow server) already wires in axum
adapters for A2A, MCP, and ACP, plus an `outbound_http_client` factory
for the pure-Rust connectors. The Harn-native Harness wants the *same*
shape but with the request/response cycle expressible in `.harn`.

Inbound HMAC verification helpers (`hmac::secure_eq`, etc.) are exposed
to scripts via `constant_time_eq` and `hmac_sha256`, so a script-side
webhook receiver can already verify GitHub / Slack / Stripe signatures
without dropping into Rust. The remaining gap is the *server* shell
itself.

## Gaps with sub-tickets filed

Every row below maps a Rust escape hatch the Harness reference impl
would otherwise have to keep, to the sub-ticket that scopes its
stdlib replacement.

| # | Gap | Sub-ticket |
| - | --- | ---------- |
| 1 | HTTP server hardening: routing, request/response builder, body limits, middleware, raw-body access | [#638](https://github.com/burin-labs/harn/issues/638) |
| 2 | WebSocket server primitives: upgrade route, frame send/receive, max-message, idle timeout | [#639](https://github.com/burin-labs/harn/issues/639) |
| 3 | Postgres client: pool, parameterized queries, transactions, RLS-style `set_config` | [#640](https://github.com/burin-labs/harn/issues/640) |
| 4 | TLS listener config: edge / self-signed-dev / PEM modes, startup-time errors | [#641](https://github.com/burin-labs/harn/issues/641) |
| 5 | `multipart/form-data` parser for inbound bodies | [#642](https://github.com/burin-labs/harn/issues/642) |
| 6 | Cookie / session helpers: parse `Cookie`, serialize `Set-Cookie`, signed sessions | [#643](https://github.com/burin-labs/harn/issues/643) |
| 7 | SSE server primitives: response builder, event writer, disconnect observation | [#644](https://github.com/burin-labs/harn/issues/644) |
| 8 | Signed-URL helper for receipt / artifact links | [#645](https://github.com/burin-labs/harn/issues/645) |
| 9 | Egress allowlist enforcement for outbound HTTP / SSE / WS / connector calls | [#647](https://github.com/burin-labs/harn/issues/647) |

Sub-tickets 1–8 were filed by a parallel session that ran the same
audit on the same day; this PR adopts them as-is and adds the egress
allowlist (9) which had not been captured.

Each sub-ticket carries the same boilerplate done-criteria:

- functions register through the authoritative stdlib registry
- mdbook docs include a worked example
- linter / typechecker / highlighter consumers derive names from live
  stdlib registration (no separate hardcoded list)
- if builtins or keywords change, `make gen-highlight` regenerates
  `docs/theme/harn-keywords.js` (the pre-commit hook re-stages it)
- tests cover routing / parsing / rejection / cancellation as
  appropriate

## Crypto follow-ups (not blocking #634)

These are out of scope for the #634 explicit list but worth noting so
the next pass can decide whether to file:

- `hmac_sha512`, `hmac_sha384`, `hmac_sha1` — legacy webhook providers
  still emit SHA-1 (Bitbucket, some older signing schemes); the
  `Connector` HMAC helper underneath is generic, only the script-facing
  builtin is SHA-256-only.
- `random_bytes(n)` — current `random` returns `f64`. Cryptographic
  callers (signed-URL nonces, session-cookie IVs, idempotency keys)
  need raw CSPRNG bytes; today they have to compose `sha256(uuid())`
  which is awkward.
- `jwt_verify(alg, token, public_key) -> claims | error` — `jwt_sign`
  exists, but a Harn-native Harness verifying inbound bearer tokens
  has no script-side verifier and falls back to provider-specific
  paths.

If a follow-up issue lands, it should be a sibling of #634, not a
child, so it does not block the audit close-out.

## Cross-references (already merged primitives we build on)

These are merged and form the substrate the new sub-tickets layer on:

- [#347](https://github.com/burin-labs/harn/issues/347) — Bytes value type and raw inbound body access on `TriggerEvent`
- [#349](https://github.com/burin-labs/harn/issues/349) — Encoding builtins (base64url, hex, base32)
- [#167](https://github.com/burin-labs/harn/issues/167) — Connector trait + HMAC verify helper
- [#168](https://github.com/burin-labs/harn/issues/168) — Generic webhook receiver
- [#178](https://github.com/burin-labs/harn/issues/178),
  [#179](https://github.com/burin-labs/harn/issues/179),
  [#180](https://github.com/burin-labs/harn/issues/180) —
  Orchestrator HTTP listener + TLS + Auth middleware
- [#189](https://github.com/burin-labs/harn/issues/189) — ACP-over-WebSocket primitive
- [#293](https://github.com/burin-labs/harn/issues/293) — MCP server-mode adapter
- [#466](https://github.com/burin-labs/harn/issues/466) — Pooled HTTP / SSE / WS transport primitives
- [#287](https://github.com/burin-labs/harn/issues/287),
  [#280](https://github.com/burin-labs/harn/issues/280) —
  Streaming reactive primitives + Postgres CDC

The Harness reference impl should reuse these directly rather than
re-spec them.

## Out of scope for this audit

- harn-cloud-specific tenant model, persona policy, receipt
  authorization rules
- ACME / certificate issuance automation (track separately if a
  deployment ticket needs it)
- DNS-rebinding hardening beyond connect-time host resolution (call
  out separately if it becomes load-bearing for the Harness)
- mTLS / SPIFFE outbound identity (separate primitive)
- Migration of harn-cloud SQLx schemas — schemas stay host-owned

## Closing #634

The parent issue can close once:

- sub-tickets 1–9 are filed (done as of this PR)
- the audit doc is checked in (this PR)
- the protocol epic ([#631](https://github.com/burin-labs/harn/issues/631))
  has explicit notes on which sub-tickets gate which conformance
  level (deferred to the narrative-spec ticket
  [#632](https://github.com/burin-labs/harn/issues/632) so the spec is
  the authoritative cross-reference)
