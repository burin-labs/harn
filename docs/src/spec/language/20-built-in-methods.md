<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Built-in methods

### String methods

| Method | Signature | Returns |
|---|---|---|
| `count` | `.count` (property) | int -- character count |
| `empty` | `.empty` (property) | bool -- true if empty |
| `contains(sub)` | string | bool |
| `replace(old, new)` | string, string | string |
| `split(sep)` | string | list of strings |
| `trim()` | (none) | string -- whitespace stripped |
| `starts_with(prefix)` | string | bool |
| `ends_with(suffix)` | string | bool |
| `lowercase()` | (none) | string |
| `uppercase()` | (none) | string |
| `substring(start, end?)` | int, int? | string -- character range |
| `chars()` | (none) | list of single-character strings |

`chars()` (also the `chars(text)` builtin) materializes a string into a list of
single-character strings in one linear pass. Because a string is UTF-8, random
character access (`s[i]`, `s[a:b]`, `.count`, `substring(...)`) is O(n) per call;
materialize once with `chars()` and scan the list with O(1) indexing when writing
cursor-style source scanners.

### List methods

| Method | Signature | Returns |
|---|---|---|
| `count` | (property) | int |
| `empty` | (property) | bool |
| `first` | (property) | value or nil |
| `last` | (property) | value or nil |
| `map(closure)` | closure(item) -> value | list |
| `filter(closure)` | closure(item) -> bool | list |
| `reduce(init, closure)` | value, closure(acc, item) -> value | value |
| `find(closure)` | closure(item) -> bool | value or nil |
| `any(closure)` | closure(item) -> bool | bool |
| `all(closure)` | closure(item) -> bool | bool |
| `flat_map(closure)` | closure(item) -> value/list | list (flattened) |

### Dict methods

| Method | Signature | Returns |
|---|---|---|
| `keys()` | (none) | list of strings (sorted) |
| `values()` | (none) | list of values (sorted by key) |
| `entries()` | (none) | list of `{key, value}` dicts (sorted by key) |
| `count` | (property) | int |
| `has(key)` | string | bool |
| `merge(other)` | dict | dict (other wins on conflict) |
| `map_values(closure)` | closure(value) -> value | dict |
| `filter(closure)` | closure(value) -> bool | dict |

### Dict property access

`dict.name` returns the value for key `"name"`, or `nil` if absent.

### Set builtins

Sets are created with the `set()` builtin and are immutable -- mutation
operations return a new set. Sets deduplicate values using structural
equality.

| Function | Signature | Returns |
|---|---|---|
| `set(...)` | values or a list | set -- deduplicated |
| `set_add(s, value)` | set, value | set -- with value added |
| `set_remove(s, value)` | set, value | set -- with value removed |
| `set_contains(s, value)` | set, value | bool |
| `set_union(a, b)` | set, set | set -- all items from both |
| `set_intersect(a, b)` | set, set | set -- items in both |
| `set_difference(a, b)` | set, set | set -- items in a but not b |
| `to_list(s)` | set | list -- convert set to list |

Sets are iterable with `for ... in` and support `len()`.

### Encoding and hashing builtins

| Function | Description |
|---|---|
| `base64_encode(str)` | Returns the base64-encoded version of `str` |
| `base64_decode(str)` | Returns the decoded string from a base64-encoded `str` |
| `base64url_encode(str)` | Returns the URL-safe base64 encoding of `str` using the RFC 4648 alphabet without padding |
| `base64url_decode(str)` | Returns the decoded string from a URL-safe base64 `str` without padding |
| `base32_encode(str)` | Returns the RFC 4648 base32 encoding of `str` |
| `base32_decode(str)` | Returns the decoded string from a base32-encoded `str` |
| `hex_encode(str)` | Returns the lowercase hex encoding of `str` |
| `hex_decode(str)` | Returns the decoded string from a hex-encoded `str` |
| `bytes_from_string(str)` | UTF-8 encodes `str` into `bytes` |
| `bytes_to_string(bytes)` | UTF-8 decodes `bytes` into `string` |
| `bytes_to_string_lossy(bytes)` | Lossy UTF-8 decode of `bytes` |
| `bytes_from_hex(str)` | Parses lowercase/uppercase hex into `bytes` |
| `bytes_to_hex(bytes)` | Hex-encodes `bytes` |
| `bytes_from_base64(str)` | Decodes base64 into `bytes` |
| `bytes_to_base64(bytes)` | Encodes `bytes` as base64 |
| `bytes_len(bytes)` | Returns the length in octets |
| `bytes_concat(a, b)` | Concatenates two byte buffers |
| `bytes_slice(bytes, start, end)` | Returns a clamped slice of a byte buffer |
| `bytes_eq(a, b)` | Constant-time byte equality check |
| `sha256(str)` | Returns the hex-encoded SHA-256 hash of `str` |
| `md5(str)` | Returns the hex-encoded MD5 hash of `str` |
| `hmac_sha256(key, message)` | Returns HMAC-SHA256 as lowercase hex |
| `hmac_sha256_base64(key, message)` | Returns HMAC-SHA256 as standard base64 |
| `constant_time_eq(a, b)` | Constant-time string equality for signatures |
| `signed_url(base, claims, secret, expires_at, options?)` | Returns a short-lived HMAC-SHA256 signed absolute URL or absolute path |
| `verify_signed_url(url, secret_or_keys, now, options?)` | Verifies a signed URL/path and returns `{valid, reason, signature_valid, expired, expires_at, kid, claims}` |
| `jwt_sign(alg, claims, private_key)` | Signs a compact JWT/JWS with a PEM private key. Supports `ES256` and `RS256` |
| `gzip_encode(bytes_or_string, level?)` | Gzip-compresses bytes/string into `bytes`; `level` defaults to `6` and must be `0..9` |
| `gzip_decode(bytes)` | Gzip-decompresses `bytes` and returns `bytes` |
| `zstd_encode(bytes_or_string, level?)` | Zstd-compresses bytes/string into `bytes`; `level` defaults to `3` |
| `zstd_decode(bytes)` | Zstd-decompresses `bytes` and returns `bytes` |
| `brotli_encode(bytes_or_string, quality?)` | Brotli-compresses bytes/string into `bytes`; `quality` defaults to `11` and must be `0..11` |
| `brotli_decode(bytes)` | Brotli-decompresses `bytes` and returns `bytes` |
| `tar_create(entries)` | Creates an in-memory tar archive from `[{path, content, mode?}]` and returns `bytes`; `content` may be bytes or string |
| `tar_extract(bytes)` | Extracts an in-memory tar archive into `[{path, content: bytes, mode}]` |
| `zip_create(entries)` | Creates an in-memory deflated zip archive from `[{path, content}]` and returns `bytes`; `content` may be bytes or string |
| `zip_extract(bytes)` | Extracts an in-memory zip archive into `[{path, content: bytes}]` |
| `multipart_parse(body, content_type, opts?)` | Parses a buffered `multipart/form-data` body from bytes/string plus `Content-Type`; `opts` supports `max_total_bytes`, `max_field_bytes`, and `max_fields` |
| `multipart_field_bytes(field)` | Returns a parsed multipart field's raw bytes |
| `multipart_field_text(field)` | Decodes a parsed multipart field's bytes as UTF-8, throwing on invalid text |
| `multipart_form_data(fields, opts?)` | Deterministically builds `{content_type, boundary, body}` multipart fixtures for tests; field content may be bytes or string |

```harn
let encoded = base64_encode("hello world")  // "aGVsbG8gd29ybGQ="
let decoded = base64_decode(encoded)        // "hello world"
let jwt = base64url_encode("{\"alg\":\"HS256\"}") // no `=` padding
let text = hex_decode("68656c6c6f")         // "hello"
let hash = sha256("hello")                  // hex string
let md5hash = md5("hello")                  // hex string
let gz = gzip_encode("hello")               // bytes
let hello = bytes_to_string(gzip_decode(gz)) // "hello"
```

`multipart_parse` returns `{boundary, fields, field_count, total_bytes}`.
Each field is `{name, filename, content_type, headers, bytes, text}`. `text`
is `nil` for non-UTF-8 uploads so binary data remains lossless in `bytes`.

```harn
let fixture = multipart_form_data([
  {name: "title", content: "Quarterly report"},
  {
    name: "upload",
    filename: "report.bin",
    content_type: "application/octet-stream",
    content: bytes_from_hex("000102ff"),
  },
])

let form = multipart_parse(fixture.body, fixture.content_type, {
  max_total_bytes: 1048576,
  max_field_bytes: 262144,
  max_fields: 8,
})

let title = multipart_field_text(form.fields[0])
let uploaded = multipart_field_bytes(form.fields[1])
log(title)
log(bytes_to_hex(uploaded))
```

`signed_url` is the canonical helper for short-lived Harn-hosted receipt and
artifact links. `base` may be an absolute URL with a host or an absolute path
beginning with `/`. The helper merges existing query parameters with `claims`,
adds `exp`, an optional `kid`, and a `sig`, then signs a versioned canonical
payload with HMAC-SHA256. Query canonicalization percent-encodes each key/value
with the RFC 3986 unreserved set left plain and sorts encoded pairs
lexicographically. Path canonicalization preserves `/`, preserves existing
`%XX` escapes with uppercase hex, and percent-encodes other non-unreserved
bytes. Signatures use URL-safe base64 without padding. `verify_signed_url`
removes `sig`, rebuilds the same canonical payload, compares signatures in
constant time, applies `skew_seconds` if provided, and supports key rotation by
accepting either one secret string or a `{kid: secret}` dict. Parameter names
can be overridden with `signature_param`, `expires_param`, and `kid_param`.

`jwt_sign` requires `claims` to be a dict so it can be serialized as a JSON
claims object. `ES256` expects a P-256 EC private key in PEM form; `RS256`
expects an RSA private key in PEM form. Unsupported algorithms, non-dict
claims, and invalid PEM keys throw runtime errors.

### Cookie and session builtins

| Function | Description |
|---|---|
| `cookie_parse(headers)` | Parses request `Cookie` header strings, lists, or header dicts into `{cookies, pairs, duplicates, invalid}` |
| `cookie_serialize(name, value, options?)` | Serializes one `Set-Cookie` header value. Options support `HttpOnly`, `Secure`, `SameSite`, `Path`, `Domain`, `Max-Age`, and `Expires` through snake_case or header-style keys |
| `cookie_delete(name, options?)` | Serializes a deletion cookie with `Max-Age=0` and an epoch `Expires` timestamp |
| `cookie_sign(value, secret)` / `cookie_verify(value, secret)` | Signs and verifies a string cookie value using HMAC-SHA256 with URL-safe base64 signatures |
| `session_sign(payload, secret)` / `session_verify(token, secret)` | Signs and verifies a stateless JSON session payload. Verification returns `{ok, payload, error}` and does not throw on bad signatures |
| `session_cookie(name, payload, secret, options?)` | Serializes a signed session cookie with secure defaults: `Path=/`, `HttpOnly`, `Secure`, and `SameSite=Lax` |
| `session_from_cookies(headers, name, secret)` | Parses request cookies and verifies the named cookie as a stateless session token |
| `cookie_round_trip(request_cookie?, set_cookie)` | Test helper that applies response `Set-Cookie` headers to a request cookie header and returns the next `{cookie_header, cookies}` |

Duplicate request cookies are deterministic: `cookies[name]` keeps the first
valid value in wire order and `duplicates[name]` records every observed value
for that name. Invalid cookie segments are skipped and returned in `invalid`.

`cookie_serialize` validates names and values and rejects `SameSite=None`
without `Secure`. `session_cookie` uses secure dashboard/operator defaults by
default; callers may override them explicitly for local testing.

Stateless signed sessions put the trusted payload in the cookie token. A
store-backed session should instead place only an opaque session ID in the
cookie and keep mutable state in `store_*`, `shared_map_*`, or an application
database.

### Regex builtins

| Function | Description |
|---|---|
| `regex_match(pattern, text, flags?)` | Returns a list of all non-overlapping matches, or `nil` if none match |
| `regex_replace(pattern, replacement, text, flags?)` | Replaces all matches of `pattern` in `text` |
| `regex_captures(pattern, text, flags?)` | Returns a list of capture group dicts for all matches |
| `regex_split(text, pattern, flags?)` | Splits `text` by regex matches |

The optional `flags` string supports `i` (case-insensitive), `m`
(multi-line anchors), `s` (dot matches newlines), and `x` (ignore
pattern whitespace). Regex inline flags such as `(?is)` use the same
underlying semantics.

#### regex_captures

`regex_captures(pattern, text)` finds all matches of `pattern` in `text`
and returns a list of dicts, one per match. Each dict contains:

- `match`: the full match string
- `groups`: a list of positional capture group values (from `(...)`), in
  source order and excluding the full match. Unmatched optional groups are
  `nil`.
- `start` / `end`: character (code-point) offsets of the full match in
  `text`
- `line`: 1-based line number of the match start
- Any named capture groups (from `(?P<name>...)`) as additional keys

```harn
let results = regex_captures("(\\w+)@(\\w+)", "alice@example bob@test")
// results == [
//   {match: "alice@example", groups: ["alice", "example"], start: 0, end: 13, line: 1},
//   {match: "bob@test", groups: ["bob", "test"], start: 14, end: 22, line: 1}
// ]

let named = regex_captures("(?P<user>\\w+):(?P<role>\\w+)", "alice:admin")
// named == [{match: "alice:admin", groups: ["alice", "admin"], user: "alice", role: "admin"}]

let body = regex_captures("(?is)<body\\b[^>]*>(.*?)</body>", html)
let also_body = regex_captures("<body\\b[^>]*>(.*?)</body>", html, "is")
```

Returns an empty list if there are no matches.

Regex patterns are compiled and cached internally using a thread-local
cache. Repeated calls with the same pattern string reuse the compiled
regex, avoiding recompilation overhead. This is a performance optimization
with no API-visible change.

### Connector interop builtins

The orchestrator exposes connector-oriented builtins for manifest-driven
provider integrations.

| Function | Description |
|---|---|
| `connector_call(provider, method, params?)` | Invoke the active outbound connector client for `provider` and return JSON-like result data |
| `egress_policy(config)` | Install a process egress policy for HTTP, SSE, WebSocket, and Rust-backed connector outbound calls. Rules support exact hosts, `*.suffix` hosts, IP literals/CIDR, optional `:port`, and `default: "deny"` allowlist mode. `block_private: "private"` blocks DNS-resolved loopback, private, link-local, metadata, multicast, documentation, CGNAT, benchmark, and equivalent IPv6 ranges; `block_private: "off"` opts out, and `allow_loopback: true` opens only loopback. The same axes can be seeded with `HARN_EGRESS_BLOCK_PRIVATE` and `HARN_EGRESS_ALLOW_LOOPBACK`. Blocked calls throw `{type: "EgressBlocked", category: "egress_blocked", host, port, reason, url}` |
| `secret_get(secret_id)` | Read a secret from the active connector context. Only available while executing a Harn-backed connector export such as `normalize_inbound` or `call` |
| `event_log_emit(topic, kind, payload, headers?)` | Append an event to the active event log from a Harn-backed connector export |
| `metrics_inc(name, amount?)` | Increment a connector-owned Prometheus counter from a Harn-backed connector export |

Harn-backed connector modules are loaded through manifest `[[providers]]`
entries and must export `provider_id()`, `kinds()`, and `payload_schema()`.
Inbound providers also export `normalize_inbound(raw)`, which returns a
`NormalizeResult` v1 dict. The top-level `type` field is one of:

- `"event"` with `event: {kind, dedupe_key, payload, ...}` for one normalized
  event.
- `"batch"` with `events: [{kind, dedupe_key, payload, ...}, ...]` for multiple
  normalized events.
- `"immediate_response"` with `immediate_response: {status, headers?, body?}`
  and optional `event` or `events` fields for ack-first webhook responses that
  may still enqueue normalized events.
- `"reject"` with `status`, `headers?`, and `body?` for explicit verification
  or unsupported-input rejection.

Each normalized event includes `kind`, `dedupe_key`, and `payload` plus optional
metadata such as `occurred_at`, `tenant_id`, `headers`, `batch`, and
`signature_status`. During the transition to NormalizeResult v1, runtimes also
accept the legacy direct event dict shape.

Harn connector exports run under an effect policy chosen by export name.
`normalize_inbound` uses the hot-path local class by default: deterministic
stdlib work, JSON/base64/body handling, signature verification, `secret_get`,
`event_log_emit`, and `metrics_inc` are allowed, while outbound network,
`connector_call`, LLM calls, process execution, host/MCP calls, and ambient
filesystem/project access are denied before the effect runs. `poll_tick` and
`call` use the connector-outbound class, which allows network and
`connector_call` but keeps filesystem, process, host/MCP, and LLM effects
denied by default. `activate` uses the activation class with the same default
restrictions as connector-outbound setup. Hosts may override these defaults for
trusted private connectors by supplying an explicit export policy.

Provider entries may declare setup OAuth metadata. This metadata is consumed by
`harn connect <provider>` so connector packages can describe their OAuth
surface without adding provider-specific Rust code:

```toml
[[providers]]
id = "acme"
connector = { harn = ".harn/packages/acme-connector/lib.harn" }
oauth = {
  resource = "https://api.acme.example/",
  authorization_endpoint = "https://auth.acme.example/oauth/authorize",
  token_endpoint = "https://auth.acme.example/oauth/token",
  registration_endpoint = "https://auth.acme.example/oauth/register",
  scopes = "acme.read acme.write",
  token_endpoint_auth_method = "none",
}
```

`resource` is required unless the operator supplies `--resource`. The endpoint
fields may be omitted for compliant protected resources that advertise OAuth
metadata. `client_id`, `client_secret`, and `token_endpoint_auth_method` are
defaults only; CLI flags override them for a single connect run.

Poll-capable providers export `poll_tick(ctx)`. The orchestrator invokes this
hook for `kind = "poll"` bindings using the binding's `poll` configuration:
`interval`/`interval_ms`/`interval_secs`, optional
`jitter`/`jitter_ms`/`jitter_secs`, `state_key` or `cursor_state_key`,
`tenant_id`, `lease_id`, and `max_batch_size`. `ctx` contains the activated
binding, `binding_id`, RFC3339 `tick_at`, prior `cursor`, prior connector
`state`, `state_key`, optional `tenant_id`, `{id, tenant_id}` lease metadata,
and optional `max_batch_size`. `poll_tick` returns either a list of normalized
event dicts or `{events, cursor?, state?}`. Returned events enter the same
post-normalize dedupe and trigger inbox path as connector ingress events, and
the returned cursor/state is persisted for the next tick.

