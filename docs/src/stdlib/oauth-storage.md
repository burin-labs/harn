# OAuth storage stdlib

`std/oauth/storage` is the token-store abstraction shared by the OAuth
client (`OAuth.client(...)`, RFC 6749 + 7636, RFC 8628 device flow). One
storage handle backs every grant type, every refresh, and every revoke
the client performs. A handle is just a dict with three closures —
`get`, `set`, `delete` — so the client never needs to know whether a
token lives in process memory, on disk, in `harn-cloud`, or in a vault.

The five backends are:

| Constructor | Backing | Persists past `harn run`? | Best for |
|---|---|---|---|
| `memory()` | per-VM `BTreeMap` | no | short-lived agents, ephemeral scripts |
| `file(path, encryption_key)` | one AES-256-GCM-sealed file | yes | local development, single-host operators |
| `harn_cloud_session()` | host capability, per-session | yes (cloud) | a single user's cloud-managed agent |
| `harn_cloud_org()` | host capability, org-scoped | yes (cloud) | shared org credentials ("the org's GitHub bot") |
| `custom(handlers)` | caller-supplied closures | depends | vaults, KMS-backed key/value stores |

The shape of a stored entry is a `TokenSet`:

```harn
type TokenSet = {
  access_token: string,
  refresh_token?: string,
  expires_at_unix?: int,
  token_type?: string,
  scopes?: list<string>,
  metadata?: dict,
}
```

## Calling the API

```harn
import { memory } from "std/oauth/storage"

pipeline default() {
  let store = memory()
  store.set(
    "github",
    {access_token: "abc", refresh_token: "rfr", expires_at_unix: 17000000000},
    3600,
  )
  let token = store.get("github")              // -> TokenSet | nil
  if token != nil {
    log("auth: Bearer " + token.access_token)
  }
  store.delete("github")
}
```

The OAuth client takes a `storage` option that accepts any of these
handles unchanged:

```harn
import { Providers } from "std/oauth/providers"
import { harn_cloud_org } from "std/oauth/storage"

let client = OAuth.client(Providers.github, {
  client_id: env("GITHUB_OAUTH_CLIENT_ID"),
  scopes: ["repo"],
  storage: harn_cloud_org(),
})
```

`storage()` returns the namespace dict matching the OAuth epic's
`OAuth.Storage.*` shape, with `memory` / `harn_cloud_*` as ready
handles and `file` / `custom` as factory closures.

## File backend

`file(path, encryption_key)` writes a single envelope to `path`:

```json
{ "version": 1, "nonce": "<base64>", "ciphertext": "<base64>" }
```

The 32-byte AES-256-GCM key is derived from `encryption_key` via
HKDF-SHA256 (`info = "harn-oauth-storage-v1"`). Pass high-entropy bytes
or a string sourced from a KMS or `crypto.random_bytes(32)` — not a
user passphrase. A fresh random nonce is sampled on every write, and
the file is renamed into place from a sibling `.tmp` so partial writes
never corrupt existing tokens.

Decryption with the wrong key raises an error rather than returning a
plausible-looking but wrong TokenSet. Deleting the final entry removes
the file entirely so an `ls` on the storage directory reflects the
absence of tokens.

## Cloud backends

`harn_cloud_session()` and `harn_cloud_org()` route every call through
the `oauth_storage` host capability:

| Operation | Params | Returns |
|---|---|---|
| `oauth_storage.cloud_get` | `{scope, key}` | TokenSet or nil |
| `oauth_storage.cloud_set` | `{scope, key, token, ttl_seconds?}` | nil |
| `oauth_storage.cloud_delete` | `{scope, key}` | nil |

`scope` is `"session"` for `harn_cloud_session()` and `"org"` for
`harn_cloud_org()`. The harn-cloud embedder is responsible for
tenant-scoped storage (RLS) and refresh metadata.

Tests can substitute the host with `host_mock("oauth_storage",
"cloud_get", {result: ...})`. With no embedder and no mock, the call
raises `oauth_storage.cloud_get is not available`, which is the
deterministic "not configured" signal.

## Custom backend hook protocol

`custom(handlers)` lets callers plug in a vault, KMS, or
platform-native keychain. `handlers` is a dict with three keys:

```harn
import { custom } from "std/oauth/storage"

let store = custom({
  get: { key -> vault_get("oauth/" + key) },
  set: { key, token_set, ttl_seconds = nil ->
    vault_put("oauth/" + key, token_set, {ttl: ttl_seconds})
  },
  delete: { key -> vault_delete("oauth/" + key) },
  id: "my-vault",          // optional, surfaces in diagnostics
})
```

Contract for each closure:

| Closure | Signature | Returns |
|---|---|---|
| `get` | `fn(key: string) -> TokenSet \| nil` | The stored TokenSet, or `nil` if absent. **Must not raise** for a missing key. |
| `set` | `fn(key: string, token_set: TokenSet, ttl_seconds: int \| nil) -> nil` | Persists the TokenSet. `ttl_seconds` is a hint; backends may ignore it. |
| `delete` | `fn(key: string) -> nil` | Idempotent removal. **Must not raise** for a missing key. |

`custom` validates that all three closures are functions before
returning the handle. The optional `id` field defaults to `"custom"`
and is exposed as `store.id` so diagnostics can distinguish multiple
custom backends.

Closure capture is by value in Harn, so the closures cannot mutate
outer scope to track state. Delegate to a real backend (`harn-cloud`,
the file backend, an HTTP API, an MCP tool) inside the closures
instead.

## Acceptance criteria mapping

This module satisfies the OA-03 acceptance criteria from issue #1904:

* Five backends implemented (`memory`, `file`, `harn_cloud_session`,
  `harn_cloud_org`, `custom`).
* File backend encrypts at rest with AES-256-GCM.
* Cloud backends route through `oauth_storage.cloud_*` host
  capabilities so embedders enforce RLS / tenant isolation.
* Custom backend hook protocol documented above.
* Conformance per backend (store / retrieve / refresh / delete) lives
  in `conformance/tests/stdlib/oauth_storage.harn`.
