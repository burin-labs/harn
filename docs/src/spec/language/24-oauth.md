<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## OAuth

Programs may compose OAuth flows by importing from `std/oauth/*`. The
stack is portable across providers: there is no provider-specific Rust
code in the language runtime.

| Module | Purpose |
|---|---|
| `std/oauth/providers` | Provider catalogue (10 built-ins plus `custom(...)` + `github_enterprise(...)`). Each record carries `auth_url`, `token_url`, `device_code_url?`, `revoke_url?`, `userinfo_url?`, `default_scopes`, `pkce_required`, `refresh_handling`, and `documented_quirks`. |
| `std/oauth/token_exchange_catalog` | Shipped RFC 8693 authorization-server capability row data. |
| `std/oauth/token_exchange` | RFC 8693 token-exchange grant URI/token-type helpers, overlayable row loading, and nested `act` claim helpers. |
| `std/oauth/storage` | Token-storage trait with five interchangeable backends (`memory`, `file`, `harn_cloud_session`, `harn_cloud_org`, `custom`). Each backend exposes `get(key) -> TokenSet \| nil`, `set(key, token_set, ttl_seconds = nil) -> nil`, and `delete(key) -> nil`. |
| `std/oauth/client` | Authorization-code grant with mandatory PKCE S256 and CSRF `state`, RFC 8693 token exchange, transparent refresh at >=75% TTL, RFC 7009 best-effort `revoke`, and a 401-bounded one-shot retry on `request(...)`. Refreshes that fail emit diagnostic `HARN-OAU-002`. |
| `std/oauth/device_flow` | RFC 8628 device authorization grant for headless contexts. Persists the resulting TokenSet into the same storage backend the authorization-code client reads from. |
| `std/oauth/dynamic_registration` | RFC 7591 client registration + RFC 8414 authorization-server metadata. Validation errors carry diagnostic `HARN-OAU-005`. |
| `std/oauth/redaction` | Per-thread custom regex pattern registration on top of the runtime's default token-shape catalog. Each match emits diagnostic `HARN-OAU-001` to the audit ring drained via `drain_audit()`. |

Successful exchanges and refreshes emit audit events on the
`oauth.client.audit`, `oauth.device_flow.audit`, and
`oauth.dynreg.audit` event-log topics. Audit payloads carry presence
flags + expiry timestamps and never include access tokens, refresh
tokens, device codes, user codes, or registered `client_secret`s.

The user-facing reference, including per-provider cookbook recipes, is
`docs/src/oauth.md`. The CLI surface (`harn connect <provider>`) is
documented in `docs/src/orchestrator/oauth.md`.

