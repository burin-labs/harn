# OAuth

Harn ships a complete OAuth 2.x client stack as part of the standard
library. One handle covers the human authorization-code dance, headless
device flow, transparent refresh, server-side dynamic client
registration, pluggable token storage, and token-shape redaction. Every
piece is RFC-shaped so it slots into existing identity providers
without provider-specific Rust code.

| Module | RFC | Purpose |
|---|---|---|
| `std/oauth/providers` | — | Catalogue of preconfigured providers + `custom(...)` factory |
| `std/oauth/token_exchange_catalog` | 8693 | Shipped token-exchange capability row data |
| `std/oauth/token_exchange` | 8693 | Token-exchange row loading + `act` claim helpers |
| `std/oauth/storage` | — | Five interchangeable token stores (memory, file, cloud session/org, custom) |
| `std/oauth/client` | 6749 + 7636 + 8693 + 9700 | Authorization-code flow with PKCE S256, RFC 8693 token exchange, transparent refresh, 401-retry |
| `std/oauth/device_flow` | 8628 | Headless device authorization grant |
| `std/oauth/dynamic_registration` | 7591 + 8414 | Worker-side metadata + dynamic client registration |
| `std/oauth/redaction` | — | OAuth-token catalog + `HARN-OAU-001` audit ring |

The CLI surface (`harn connect <provider>`) is documented separately in
[Connector OAuth](./orchestrator/oauth.md); this page covers the
scripting API for code that runs *inside* a Harn pipeline.

## The 30-second tour

```harn,ignore
import { providers } from "std/oauth/providers"
import { memory } from "std/oauth/storage"
import { client, exchange_code, request, start_authorization, token, token_exchange } from "std/oauth/client"

const cli = client(
  providers().github,
  {
    client_id: env("GITHUB_OAUTH_CLIENT_ID"),
    client_secret: env("GITHUB_OAUTH_CLIENT_SECRET"),
    scopes: ["read:user", "user:email"],
    redirect_uri: "http://127.0.0.1:8765/callback",
    storage: memory(),
  },
)

// One-time: send a human through the browser dance.
const pkce = start_authorization(cli)
// (host: open pkce.url, capture redirected code+state, then)
const _ = exchange_code(cli, pkce, code, state)

// Steady state: client owns refresh + 1x retry on 401.
const res = request(cli, "GET", "https://api.github.com/user")
```

## Providers (`std/oauth/providers`)

A `Provider` is a dict that captures every endpoint, scope default, and
documented quirk the rest of the OAuth stack needs to drive a real IdP.
Built-in providers (returned by named factories or the `providers()`
namespace) are validated against vendor docs, and you can override any
field per-call:

```harn,ignore
import { atlassian, github, github_enterprise, providers } from "std/oauth/providers"

const gh = github()                                              // public github.com
const ghe = github_enterprise("https://ghe.example.com")         // enterprise server
const conf = atlassian({default_scopes: ["read:jira-work", "offline_access"]})

const all = providers()                                          // {github, slack, ..., custom, github_enterprise}
```

Built-ins: `github`, `github_enterprise`, `slack`, `linear`, `notion`,
`google`, `microsoft`, `atlassian`, `discord`, `gitlab`, `bitbucket`.
Plus `custom(config, overrides?)` for in-house IdPs.

A provider record carries:

| Field | Notes |
|---|---|
| `id` / `label` | Used as the default storage key and for diagnostics |
| `auth_url` / `token_url` | Authorization-code endpoints |
| `device_code_url?` | Present iff the provider supports RFC 8628 device flow |
| `revoke_url?` | RFC 7009 best-effort; the client always discards locally |
| `userinfo_url?` | OIDC `/userinfo` analogue when one exists |
| `default_scopes` | Used when `opts.scopes` is not set |
| `pkce_required` | Informational; PKCE is unconditionally used by the client |
| `refresh_handling` | `{strategy, refresh_grant, rotates_refresh_token, notes}` |
| `token_exchange?` | RFC 8693 support row; custom providers can opt in with data |
| `documented_quirks` | Provider-specific constraints and gotchas |
| `documentation_url` | Vendor doc link for "go read the source" |

`provider("github", overrides?)` is a string-keyed factory for the same
records, and `provider_catalog(overrides?)` returns all ten built-ins
keyed by id.

## Storage (`std/oauth/storage`)

Token storage is a single three-closure protocol:

```text
storage.get(key) -> TokenSet | nil
storage.set(key, token_set, ttl_seconds = nil) -> nil
storage.delete(key) -> nil
storage.with_refresh_lock(key, { -> ... }) -> any
```

Pick a backend; the OAuth client never knows the difference.

| Constructor | Persists? | Best for |
|---|---|---|
| `memory()` | no | tests, short-lived scripts |
| `file(path, key)` | yes (AES-256-GCM) | local development, single-host operators |
| `harn_cloud_session()` | yes (host cap) | one user's cloud-managed agent |
| `harn_cloud_org()` | yes (host cap) | shared org credentials ("the org's GitHub bot") |
| `custom({get, set, delete, with_refresh_lock?, id?})` | depends | vaults, KMS, platform keychains |

```harn,ignore
import { custom, file, harn_cloud_org, memory } from "std/oauth/storage"

const dev   = memory()
const disk  = file("/var/lib/harn/oauth.bin", env("HARN_OAUTH_KEY"))
const cloud = harn_cloud_org()                                   // org-shared bot
const vault = custom({
  get:    { key -> vault_get("oauth/" + key) },
  set:    { key, token_set, ttl_seconds = nil -> vault_put("oauth/" + key, token_set) },
  delete: { key -> vault_delete("oauth/" + key) },
  with_refresh_lock: { key, body -> vault_with_lock("oauth/" + key, body) },
})
```

Closure capture in Harn is by-value: a custom backend MUST delegate to a
real store (HTTP/MCP/vault) inside its closures. See the full reference
in [OAuth storage stdlib](./stdlib/oauth-storage.md).

## Authorization-code client (`std/oauth/client`)

`client(provider, opts)` builds a handle that owns the token lifecycle.

```harn,ignore
import {
  client,
  exchange_code,
  refresh,
  request,
  revoke,
  start_authorization,
  token,
  token_exchange,
} from "std/oauth/client"

const cli = client(provider, {
  client_id:          string,
  storage:            <storage handle>,
  client_secret?:     string,                       // confidential clients only
  scopes?:            list<string>,                 // defaults to provider.default_scopes
  redirect_uri?:      string,                       // required for start_authorization
  storage_key?:       string,                       // defaults to provider.id
  token_auth_method?: "none" | "client_secret_post" | "client_secret_basic",
  audience?:          string,                       // appended to /authorize and /device
  extra_auth_params?: dict,                         // raw passthrough on /authorize
})
```

The handle exposes seven operations. Each one is also re-exported as a
standalone helper that takes the handle as the first argument:

| Helper | Behavior |
|---|---|
| `start_authorization(cli)` | Returns `{url, state, code_verifier, code_challenge, ...}` |
| `exchange_code(cli, pkce, code, state)` | Validates PKCE + state, persists the TokenSet |
| `token(cli)` | Returns a valid access token (refresh on >=75% TTL) |
| `refresh(cli)` | Forces a refresh, ignoring TTL |
| `request(cli, method, url, opts?)` | Token-bearing HTTP with 1x 401 retry |
| `token_exchange(cli, opts)` | RFC 8693 token exchange; `actor_token` present means delegation, absent means impersonation |
| `revoke(cli)` | RFC 7009 best-effort + local storage delete |
| `cli.current_token()` | Reads the stored TokenSet without refresh |

### What the client guarantees

- **PKCE S256 is unconditional.** `start_authorization` generates a
  64-byte CSPRNG verifier (base64url-no-pad, ~86 chars) and a SHA-256
  S256 challenge. `code_challenge_method=S256` is hardcoded.
- **State always enforced.** `exchange_code` raises on `state` mismatch
  before touching the token endpoint.
- **Transparent refresh.** `token(cli)` re-reads storage every call and
  refreshes if the stored TokenSet is past 75% TTL or already expired.
  Refreshes run under `storage.with_refresh_lock(...)` and re-read inside
  that transaction, so TTL-triggered, explicit, and post-401 callers
  sharing a storage key collapse to one refresh grant.
- **One retry on 401.** `request(cli, ...)` performs a refresh and
  replays the request exactly once when the server returns 401. If
  another worker already rotated the token while this request waited for
  the storage lock, the retry uses the fresh stored token without spending
  another refresh grant.
- **Token exchange is data-gated.** `token_exchange(cli, opts)` validates
  subject, actor, and requested token types against `std/oauth/token_exchange`
  capability rows. Custom providers opt in with a `token_exchange` row; the
  grant returns a TokenSet and only persists it when `store: true` and
  `storage_key` are supplied.
- **Refresh-token preservation.** Token responses that omit a fresh
  `refresh_token` keep the prior one (relevant for Google + Slack +
  Discord, which only rotate on consent).
- **Audit log without secrets.** Refresh / exchange / revoke each emit
  `oauth.client.audit` (`token_refreshed` / `token_exchanged` /
  `token_revoked`) with presence flags + expiry timestamps. The access
  token never lands in the audit payload.
- **Storage is the source of truth.** Concurrent `token(cli)` or
  `refresh(cli)` calls may observe the same token, but only the lock
  holder issues the refresh. Waiters re-read inside the lock and reuse
  the stored rotated access/refresh token.
- **Storage key defaults to `provider.id`.** Pass `storage_key` to fan
  out multiple installations of the same provider (e.g. one GitHub OAuth
  app per tenant).

### Diagnostic codes

| Code | Source | Meaning |
|---|---|---|
| `HARN-OAU-001` | `std/oauth/redaction` | A persisted sink redacted an OAuth-shaped token |
| `HARN-OAU-002` | `std/oauth/client` | No refresh_token available; re-run authorization |
| `HARN-OAU-005` | `std/oauth/dynamic_registration` | RFC 7591 metadata validation rejected a candidate |

`HARN-OAU-002` is the signal to drive a fresh `start_authorization` (or
`device_flow`) — refresh failure is terminal until human consent runs
again.

## Token exchange (`std/oauth/token_exchange`)

RFC 8693 lets an OAuth client trade one token for another at the token
endpoint. Harn keeps provider support in overlayable data rows under
`std/oauth/token_exchange_catalog`, not provider-specific code.

```harn,ignore
import { client, token_exchange } from "std/oauth/client"
import { custom } from "std/oauth/providers"
import { memory } from "std/oauth/storage"
import { delegated_claims, token_type } from "std/oauth/token_exchange"

const provider = custom({
  id: "enterprise-as",
  auth_url: "https://idp.example/authorize",
  token_url: "https://idp.example/token",
  token_exchange: {
    supported: true,
    token_url: "https://idp.example/token",
    subject_token_types: [token_type("access_token")],
    actor_token_types: [token_type("jwt")],
    requested_token_types: [token_type("access_token")],
    issued_token_types: [token_type("access_token")],
    delegation: true,
    impersonation: true,
  },
})

const cli = client(provider, {client_id: "agent-client", storage: memory()})
const delegated = token_exchange(cli, {
  subject_token: user_access_token,
  subject_token_type: token_type("access_token"),
  actor_token: agent_jwt,
  actor_token_type: token_type("jwt"),
  requested_token_type: token_type("access_token"),
  audience: "hr-service",
  scope: ["employee:read"],
})
```

`actor_token` present selects delegation; omitting it selects
impersonation. `resource`, `audience`, and `scope` accept the RFC 8693
targeting parameters, and `extra_params` carries deployment-specific
fields. Returned tokens are not written to the client's normal
`storage_key`; pass `{store: true, storage_key: "..."}` when the
delegated token should be persisted separately.

The companion `delegated_claims(subject_claims, actors)` helper builds
RFC 8693 nested `act` claims with actors ordered current-to-prior, so
`delegated_claims({sub: "user"}, [{sub: "svc16"}, {sub: "svc77"}])`
produces `{sub: "user", act: {sub: "svc16", act: {sub: "svc77"}}}`.

The shipped catalog also records fast-moving identity-chain drafts so provider
overlays can opt in without changing runtime code:

| Row | Detection/tracking |
| --- | --- |
| `id-jag` | Detects `urn:ietf:params:oauth:token-type:id-jag` in `identity_chaining_requested_token_types_supported` and tracks `draft-ietf-oauth-identity-assertion-authz-grant-04` plus `draft-ietf-oauth-identity-chaining-14`. |
| `txn-token` | Tracks `draft-ietf-oauth-transaction-tokens-08` and the agent-context companion `draft-araut-oauth-transaction-tokens-for-agents-02`. Uses `requested_token_type = urn:ietf:params:oauth:token-type:txn_token`. |
| `wimse-wit-wpt` | Tracking-only row for `draft-ietf-wimse-workload-creds-01` and `draft-ietf-wimse-wpt-01`. WIT/WPT are proof-of-possession workload credentials, not a bearer token-exchange profile. |

Rows expose `provider_metadata_fields` and `tracking.drafts` so applications can
surface provider capability matches, keep draft links near policy decisions, and
replace a tracking row with a stricter provider-specific overlay when an
authorization server publishes concrete support.

## Device flow (`std/oauth/device_flow`)

For CI runners, daemons, and IDE side panes that cannot redirect a
browser, `device_flow(provider, opts)` runs the full RFC 8628 dance and
persists the resulting TokenSet into the same storage backend the
authorization-code client uses.

```harn,ignore
import { device_flow } from "std/oauth/device_flow"
import { providers } from "std/oauth/providers"
import { memory } from "std/oauth/storage"

const token_set = device_flow(
  providers().github,
  {
    client_id: env("GITHUB_OAUTH_CLIENT_ID"),
    scopes: ["read:user"],
    storage: memory(),
    on_user_code: { user_code, verification_uri ->
      log("Open " + verification_uri + " and enter " + user_code)
    },
  },
)
```

`on_user_code` is optional — the default writes the URL and code to
stderr so an operator can complete the dance manually. The poll loop
honors the server-supplied `interval`, treats `authorization_pending`
as a soft retry, adds 5s on `slow_down`, and raises on `expired_token`
or `access_denied`. The TokenSet is persisted before `device_flow`
returns, so the very next `client(...)` handle that targets the same
storage + `storage_key` picks it up without further authorization.

Audit: every successful exchange emits `oauth.device_flow.audit`
`token_obtained`. The `device_code` / `user_code` are never persisted
or logged.

## Dynamic registration (`std/oauth/dynamic_registration`)

This module is the *server side* of OAuth — covers the case where Harn
acts as a resource (or auxiliary service) that other agents register
clients against. It does not itself host HTTP; embedders (a cloud platform,
`harn serve`, custom hosts) mount the returned metadata documents and
the registration handler.

```harn,ignore
import {
  authorization_server_metadata,
  client_metadata,
  dynamic_registration_store,
  register_client,
  validate_metadata,
  well_known_paths,
  well_known_response,
} from "std/oauth/dynamic_registration"
import { providers } from "std/oauth/providers"

const paths = well_known_paths()                              // {client_metadata, authorization_server_metadata, registration}
const oas = authorization_server_metadata(
  providers().github,
  {registration_endpoint: paths.registration},
)
const oas_response = well_known_response(oas)                 // {status, content_type, headers, body}

const store = dynamic_registration_store()
const body = register_client(store, {
  redirect_uris: ["https://app.example/cb"],
  client_name: "Acme Agent",
})
// body.client_id, body.client_secret (returned ONCE), body.client_id_issued_at, ...
```

`validate_metadata(metadata)` returns `{ok, errors}` against RFC 7591
§2; each error is prefixed `HARN-OAU-005:` for stable pattern matching.
Validation is strict by default — `redirect_uris` must be absolute
`https://` or loopback `http://` per RFC 8252 §7.3, and grant /
response types and `token_endpoint_auth_method` are restricted to the
spec-blessed enums.

`get_client(store, client_id)` reads a registration back without
`client_secret` — the secret is only ever returned by the original
`register_client` call.

## Redaction (`std/oauth/redaction`)

The redaction module recognizes a catalog of high-confidence token
patterns (JWT, GitHub PAT classic + fine-grained, Slack `xox*`, AWS
`AKIA`, OpenAI
`sk-`, Stripe `sk_live_`/`sk_test_`, GitLab `glpat-`, npm `npm_`,
`Authorization: Bearer ...`). Persisted transcripts, audit receipts,
OTel span attributes, and system reminders run every string through
the catalog and replace matches with `<redacted:<pattern>:<len>>`. The
original token still flows to the underlying tool — redaction is
display-only.

```harn,ignore
import {
  clear_custom_patterns,
  custom_patterns,
  default_patterns,
  drain_audit,
  redact,
  register_pattern,
} from "std/oauth/redaction"

register_pattern("acme_api_key", "\\bACME-[A-Z0-9]{12}\\b")
const display = redact("ACME-DEADBEEF1234 calling")
for entry in drain_audit() {
  // entry.code == "HARN-OAU-001"
  // entry.pattern, entry.match_count, entry.bytes_redacted
}
```

`drain_audit()` is the authoritative compliance contract — it works on
every execution backend. Audit entries are also forwarded to the live
event-sink pipeline and (when a multi-threaded Tokio runtime is
available) appended to the `audit.token_redaction` event-log topic.

## Provider cookbook

Each recipe is a complete authorization-code or device-flow snippet
plus the provider-specific gotcha you usually only discover by reading
the vendor docs.

### GitHub

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { memory } from "std/oauth/storage"

const cli = client(
  providers().github,
  {
    client_id: env("GITHUB_OAUTH_CLIENT_ID"),
    client_secret: env("GITHUB_OAUTH_CLIENT_SECRET"),
    scopes: ["read:user", "user:email", "repo"],
    redirect_uri: "http://127.0.0.1:8765/callback",
    storage: memory(),
  },
)
const pkce = start_authorization(cli)
// host: open pkce.url, capture code + state from the redirect
const _ = exchange_code(cli, pkce, code, state)
const user = request(cli, "GET", "https://api.github.com/user")
```

GitHub OAuth-app access tokens may be long-lived without a
`refresh_token`. If you need explicit expiry + refresh, register an
**expiring user-to-server token** under the OAuth app settings; the
catalog handles both shapes transparently and falls back to the
"no-refresh" branch when the token response omits `expires_in`.

GitHub device flow uses the same client — pass `providers().github`
into `device_flow(...)` instead of `client(...)`. **Note:** device flow
must be enabled on the app registration (Developer settings → OAuth
Apps → "Enable Device Flow") before the device endpoint will issue
codes.

### Slack

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { file } from "std/oauth/storage"

const cli = client(
  providers().slack,
  {
    client_id: env("SLACK_CLIENT_ID"),
    client_secret: env("SLACK_CLIENT_SECRET"),
    scopes: ["app_mentions:read", "chat:write"],
    redirect_uri: "https://app.example/oauth/slack/callback",
    storage: file("/var/lib/harn/slack.bin", env("HARN_OAUTH_KEY")),
  },
)
```

Token rotation gotcha: when Slack token rotation is enabled, the
issued `refresh_token` is **single-use**. Two concurrent `request(cli, ...)`
calls that both decide to refresh will race on the storage `set` — the
second writer wins and the first refresh is effectively wasted. The
client's 75% TTL pre-refresh window keeps the race narrow, but pin
refresh to a single worker if you have a high-fanout deployment.

Slack's bot scopes and user scopes are separate from the "Sign in with
Slack" identity scopes — pick the right scope family before requesting
consent.

### Linear

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { harn_cloud_org } from "std/oauth/storage"

const cli = client(
  providers().linear,
  {
    client_id: env("LINEAR_CLIENT_ID"),
    client_secret: env("LINEAR_CLIENT_SECRET"),
    scopes: ["read", "write", "issues:create"],
    redirect_uri: "https://app.example/oauth/linear/callback",
    storage: harn_cloud_org(),
    storage_key: "linear:" + team_id,                       // per-team isolation
  },
)
```

Two Linear quirks the catalog handles for you:

- **Scopes are comma-separated** in Linear's authorization URL (not
  space-separated like the rest of OAuth-land). The provider record
  sets the scope separator automatically.
- **User info is a GraphQL query**, not a REST endpoint. After the
  exchange, run `request(cli, "POST", "https://api.linear.app/graphql",
  {body: "{\"query\":\"{viewer{id name}}\"}"})` instead of GET-ing a
  `/me` URL.

Use `storage_key: "linear:" + team_id` to keep per-team installations
isolated under the same provider record.

### Notion

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { harn_cloud_session } from "std/oauth/storage"

const cli = client(
  providers().notion,
  {
    client_id: env("NOTION_CLIENT_ID"),
    client_secret: env("NOTION_CLIENT_SECRET"),
    redirect_uri: "https://app.example/oauth/notion/callback",
    storage: harn_cloud_session(),
    extra_auth_params: {owner: "user"},                       // user-owned public connection
  },
)
const pages = request(
  cli,
  "GET",
  "https://api.notion.com/v1/users/me",
  {headers: {"Notion-Version": "2022-06-28"}},
)
```

Two Notion-specific things to remember:

- **Database / page access is not OAuth scopes.** The user picks pages
  during the Notion page-picker flow on `/authorize`; subsequent API
  calls can only see what the user granted. Plan your UX around the
  picker, not around incremental scope upgrades.
- **`Notion-Version` is required on every API call** after the OAuth
  dance completes. Add it to the `opts.headers` you pass into `request(...)`
  or set it inside a tiny wrapper.

### Google

```harn,ignore
import { client, exchange_code, refresh, request, start_authorization } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { file } from "std/oauth/storage"

const cli = client(
  providers().google,
  {
    client_id: env("GOOGLE_CLIENT_ID"),
    client_secret: env("GOOGLE_CLIENT_SECRET"),
    scopes: ["openid", "email", "profile", "https://www.googleapis.com/auth/drive.readonly"],
    redirect_uri: "http://127.0.0.1:8765/callback",
    storage: file("/var/lib/harn/google.bin", env("HARN_OAUTH_KEY")),
    extra_auth_params: {access_type: "offline", prompt: "consent"},
  },
)
```

Google's refresh-token model is the most surprising one in the catalog:

- **Refresh tokens are only issued on first consent** (or when
  `prompt=consent` is forced). Subsequent grants reuse the existing
  refresh token. The catalog preserves the prior `refresh_token` across
  refreshes that don't include one — but if you delete the storage entry
  and re-run authorization without `prompt=consent`, you will get back
  an access token with no refresh capability.
- **Workspace consent screens** require the OAuth client app to be
  marked "Internal" or to go through verification before users outside
  the publishing project can grant the scopes.
- **Incremental authorization** is preferred for product-specific
  scopes (Gmail, Drive, Calendar). Set `extra_auth_params:
  {include_granted_scopes: "true"}` and request additional scopes via
  fresh authorization rounds instead of asking for everything up front.

### Microsoft

```harn,ignore
import { client, request, start_authorization } from "std/oauth/client"
import { microsoft } from "std/oauth/providers"
import { harn_cloud_org } from "std/oauth/storage"

const tenant = env("MS_TENANT_ID")
const cli = client(
  microsoft({
    auth_url:  "https://login.microsoftonline.com/" + tenant + "/oauth2/v2.0/authorize",
    token_url: "https://login.microsoftonline.com/" + tenant + "/oauth2/v2.0/token",
  }),
  {
    client_id: env("MS_CLIENT_ID"),
    client_secret: env("MS_CLIENT_SECRET"),
    scopes: ["openid", "profile", "email", "offline_access", "User.Read", "Mail.Read"],
    redirect_uri: "https://app.example/oauth/microsoft/callback",
    storage: harn_cloud_org(),
  },
)
const me = request(cli, "GET", "https://graph.microsoft.com/v1.0/me")
```

Microsoft Identity has two ergonomic traps:

- **Graph delegated scopes are not OIDC claims.** `openid`, `profile`,
  `email`, `offline_access` are claim scopes (your access token still
  needs them for refresh + ID-token contents). `User.Read`,
  `Mail.Read`, etc. are *resource permissions* on Microsoft Graph —
  granting one without the matching resource permission yields a token
  Graph cannot use.
- **Audience claim ≠ access token target.** The token returned for the
  Graph audience is *not* valid against custom-API audiences. If you
  also need to call a custom resource, run a second `client(...)` with
  the per-resource scopes (Microsoft does not issue multi-audience
  tokens). Use `storage_key` to keep the two TokenSets distinct.

Use a tenant-specific URL (above) when an app is single-tenant. The
default `/common` route is the right choice for multi-tenant apps; it
only resolves the user's home tenant at consent time.

### Atlassian (Jira + Confluence)

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { harn_cloud_org } from "std/oauth/storage"

const cli = client(
  providers().atlassian,
  {
    client_id: env("ATLASSIAN_CLIENT_ID"),
    client_secret: env("ATLASSIAN_CLIENT_SECRET"),
    scopes: ["read:jira-work", "read:confluence-content.summary", "offline_access"],
    redirect_uri: "https://app.example/oauth/atlassian/callback",
    storage: harn_cloud_org(),
    audience: "api.atlassian.com",
  },
)
// 1) Resolve accessible cloud sites (one token covers Jira AND Confluence on each).
const sites = request(cli, "GET", "https://api.atlassian.com/oauth/token/accessible-resources")
// 2) Use the returned cloudid for product calls:
//    https://api.atlassian.com/ex/jira/<cloudid>/rest/api/3/myself
//    https://api.atlassian.com/ex/confluence/<cloudid>/wiki/rest/api/user/current
```

Atlassian's 3LO flow is one OAuth client for both products — Jira and
Confluence share scopes, the same `audience=api.atlassian.com`, and the
same token. The provider record sets the audience for you; you only
need to request the union of scopes the agent will use across both
products.

Refresh tokens **rotate on every refresh** — the prior refresh token is
invalidated as soon as the new one is issued. The OAuth client persists
the new refresh on each successful refresh; do not cache a copy in
your own code.

### Discord

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { file } from "std/oauth/storage"

const cli = client(
  providers().discord,
  {
    client_id: env("DISCORD_CLIENT_ID"),
    client_secret: env("DISCORD_CLIENT_SECRET"),
    scopes: ["identify", "email"],                            // user scope
    redirect_uri: "https://app.example/oauth/discord/callback",
    storage: file("/var/lib/harn/discord.bin", env("HARN_OAUTH_KEY")),
  },
)
```

Discord has a sharp split between **bot tokens** (long-lived, scoped to
a guild via the `bot` scope on a single one-time installation) and
**user tokens** (the OAuth dance above, with `identify` / `email` /
`guilds` user scopes). Mixing them throws off intent: a bot token does
not respond to user-API endpoints, and a user token cannot drive bot
gateway events.

If you need both (e.g. an OAuth-authorized agent that also runs as a
bot), use two `client(...)` instances with different `storage_key`s and
keep the token tracks separate.

### GitLab (cloud + self-hosted)

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { custom, providers } from "std/oauth/providers"
import { file } from "std/oauth/storage"

// Cloud:
const cloud_cli = client(
  providers().gitlab,
  {
    client_id: env("GITLAB_CLIENT_ID"),
    client_secret: env("GITLAB_CLIENT_SECRET"),
    scopes: ["read_api", "read_user", "openid", "profile", "email"],
    redirect_uri: "https://app.example/oauth/gitlab/callback",
    storage: file("/var/lib/harn/gitlab.bin", env("HARN_OAUTH_KEY")),
  },
)

// Self-hosted: same /oauth paths under your instance base URL.
const base = "https://gitlab.acme.example"
const self_hosted = custom({
  id: "gitlab",
  label: "GitLab (self-hosted)",
  auth_url:        base + "/oauth/authorize",
  token_url:       base + "/oauth/token",
  device_code_url: base + "/oauth/authorize_device",
  revoke_url:      base + "/oauth/revoke",
  userinfo_url:    base + "/oauth/userinfo",
  default_scopes:  ["read_api", "openid"],
  pkce_required:   true,
})
```

GitLab's refresh response **rotates both tokens** — the prior access
token is invalidated alongside the prior refresh token. This is the
RFC-spec-strict behavior; the client handles it transparently. The
catch: if a refresh succeeds but the caller crashes before persisting
the new TokenSet, the old refresh token is gone. Use a durable storage
backend (`file(...)` or `harn_cloud_*()`) in production rather than
`memory()`.

Device flow is available on GitLab 17.1+ (generally available in
17.9+); the catalog enables it on `providers().gitlab` and on the
custom self-hosted record above when you target an instance that's new
enough.

### Bitbucket (workspace-scoped)

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { file } from "std/oauth/storage"

const cli = client(
  providers().bitbucket,
  {
    client_id: env("BITBUCKET_CLIENT_KEY"),
    client_secret: env("BITBUCKET_CLIENT_SECRET"),
    scopes: ["account", "repository", "issue"],
    redirect_uri: "https://app.example/oauth/bitbucket/callback",
    storage: file("/var/lib/harn/bitbucket.bin", env("HARN_OAUTH_KEY")),
  },
)
const workspaces = request(cli, "GET", "https://api.bitbucket.org/2.0/workspaces")
```

Bitbucket Cloud OAuth supports **authorization-code and
client-credentials grants only** — there is no device flow. For
workspace-scoped automation (running as a service account against a
single workspace), prefer client credentials over a long-lived user
OAuth token: register the OAuth consumer with the workspace, then
exchange `client_credentials` outside Harn's authorization-code helpers
since the grant has no human in the loop.

A refreshed Bitbucket token response includes a new refresh token that
the catalog stores automatically; the old one expires shortly after
use.

## Cross-cutting cookbook

### Headless CI agent (device flow)

```harn,ignore
import { device_flow } from "std/oauth/device_flow"
import { providers } from "std/oauth/providers"
import { file } from "std/oauth/storage"

const store = file("/var/lib/harn/ci-token.bin", env("HARN_OAUTH_KEY"))
const token_set = device_flow(
  providers().github,
  {
    client_id: env("GH_OAUTH_CLIENT_ID"),
    scopes: ["read:user", "repo"],
    storage: store,
    on_user_code: { user_code, verification_uri ->
      // Surface to the CI log + a chat webhook so an operator can complete the dance.
      const _ = log("Visit " + verification_uri + " and enter " + user_code)
      const _ = http_post(env("SLACK_WEBHOOK_URL"), json_stringify({
        text: "CI auth pending: open " + verification_uri + " and enter `" + user_code + "`",
      }), {headers: {"Content-Type": "application/json"}})
      nil
    },
  },
)
// On subsequent CI runs the file backend already has the token; skip device_flow.
```

The same pattern works for Google, Microsoft, and GitLab. Slack,
Linear, Notion, Atlassian, Discord, and Bitbucket do not advertise
device endpoints — `device_flow(...)` raises on construction if
`provider.device_code_url` is nil.

### Org-shared GitHub bot (`harn_cloud_org`)

```harn,ignore
import { client, request } from "std/oauth/client"
import { providers } from "std/oauth/providers"
import { harn_cloud_org } from "std/oauth/storage"

const cli = client(
  providers().github,
  {
    client_id: env("ORG_GITHUB_CLIENT_ID"),
    client_secret: env("ORG_GITHUB_CLIENT_SECRET"),
    scopes: ["read:org", "repo"],
    redirect_uri: env("ORG_REDIRECT_URI"),
    storage: harn_cloud_org(),
    storage_key: "github:org-bot",
  },
)
const issues = request(cli, "GET", "https://api.github.com/orgs/burin-labs/issues")
```

`harn_cloud_org()` routes through the `oauth_storage.cloud_*` host
capability with `scope = "org"`, including the refresh-lock operations
used by transparent token rotation. A cloud platform is responsible for
tenant-scoped storage (RLS), so two agents running in the same org share
the same authenticated client without either of them being able to read
tokens for a different org. The `storage_key` is per-purpose, not
per-user: one entry covers every consumer of the bot.

### Custom enterprise OIDC provider

```harn,ignore
import { client, exchange_code, request, start_authorization } from "std/oauth/client"
import { custom } from "std/oauth/providers"
import { file } from "std/oauth/storage"

const acme = custom({
  id: "acme-oidc",
  label: "Acme Enterprise OIDC",
  auth_url:     "https://idp.acme.example/oauth2/authorize",
  token_url:    "https://idp.acme.example/oauth2/token",
  revoke_url:   "https://idp.acme.example/oauth2/revoke",
  userinfo_url: "https://idp.acme.example/oauth2/userinfo",
  default_scopes: ["openid", "profile", "email", "offline_access"],
  pkce_required: true,
})
const cli = client(
  acme,
  {
    client_id: env("ACME_OIDC_CLIENT_ID"),
    client_secret: env("ACME_OIDC_CLIENT_SECRET"),
    scopes: ["openid", "profile", "email", "offline_access", "acme.api.read"],
    redirect_uri: "https://app.acme.internal/oauth/callback",
    storage: file("/var/lib/harn/acme.bin", env("HARN_OAUTH_KEY")),
    audience: "https://api.acme.example",
    extra_auth_params: {prompt: "select_account"},
  },
)
```

The fields on `custom({...})` mirror the built-in provider records. If
your IdP advertises `.well-known/openid-configuration`, copy the URLs
from there verbatim; the `refresh_handling` record can stay at the
default unless your IdP does something unusual (mTLS, JAR/JARM,
custom grant types).

For inhouse IdPs that *don't* speak OAuth 2.x (e.g. legacy SAML), use a
`custom(...)` storage backend to wrap your existing token broker
instead of teaching the OAuth client about a non-OAuth protocol.

## Related

- [OAuth storage stdlib](./stdlib/oauth-storage.md) — full storage reference.
- [Connector OAuth](./orchestrator/oauth.md) — the `harn connect`
  CLI on top of this stack.
- [Redaction policy](./redaction.md) — what's automatically scrubbed
  from persisted transcripts and receipts.
- Conformance fixtures: `conformance/tests/stdlib/oauth/oauth_*.harn`.
