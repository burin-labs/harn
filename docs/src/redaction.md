# Redaction policy

Harn writes operational data to several places that an outside party
might eventually see — `.harn-runs/*` transcripts, `Receipt` envelopes,
the JSONL event log, the portal API, connector status snapshots,
crystallized workflow bundles. Each of those surfaces was previously
responsible for its own ad-hoc scrubbing of HTTP headers, URL query
parameters, and tokens, which meant the same secret could leak through
one path while being scrubbed on another.

The [`harn_vm::redact`](https://docs.rs/harn-vm/latest/harn_vm/redact/index.html)
module collapses that into a single `RedactionPolicy` that every
persistence path consults, so a string treated as sensitive in one
place is treated as sensitive everywhere.

## Default categories

The default policy redacts:

- **Auth headers** — `Authorization`, `Cookie`, `Set-Cookie`,
  `Proxy-Authorization`, `X-API-Key`, `X-Auth-Token`,
  `X-CSRF-Token`, `X-XSRF-Token`, plus any header whose name (case-
  insensitively) contains `authorization`, `cookie`, `secret`, `token`,
  or `key`. Common debugging headers (`User-Agent`, `Content-Type`,
  `X-Request-Id`, GitHub/Slack/Linear/Notion signature headers) are
  retained on a built-in safe-list.
- **URLs with credentials in userinfo** — `https://user:pw@host/...`
  loses both `user` and `pw`. The username and password fields are
  cleared rather than replaced so downstream URL parsers continue to
  function.
- **Sensitive URL query parameters** — `api_key`, `apikey`,
  `access_token`, `refresh_token`, `id_token`, `client_secret`,
  `password`, `secret`, `token`, `auth`, `bearer`, `sig`, `signature`,
  and anything ending in `_token`, `_secret`, or `_password`.
- **JSON object fields** — recursive walk of any persisted JSON value
  swaps the value for `[redacted]` whenever the key matches the
  default-sensitive set above (or the additions a host installs via
  [`with_extra_field`](#host-extension-points)).
- **Free-form string contents** — strings are scanned for high-
  confidence secret patterns: AWS access key ids, GitHub OAuth/PAT/
  fine-grained tokens, GitLab `glpat-`, npm `npm_`, OpenAI/Anthropic
  `sk-`, Slack `xox*-`, Stripe `sk_live_`/`sk_test_`/`rk_…`, RFC 6750
  `Bearer …` tokens, and PEM `-----BEGIN … PRIVATE KEY-----` blocks.
  These regexes are the same set used by the `secret_scan` builtin —
  there is one definition, not two.
- **URL-shaped strings** — when a JSON string value is itself a single
  http(s) URL, the policy applies userinfo and query-param redaction
  to it before the secret-pattern scanner runs.

The redaction marker is the literal `"[redacted]"`. Receipts, log
events, portal DTOs, transcript JSONL, and connector envelopes all use
the same placeholder, which makes downstream parsers and humans
grepping logs trivial to write.

## Where the policy is applied

| Surface | Entry point | Notes |
| --- | --- | --- |
| LLM transcript JSONL | [`crate::llm::agent_observe`](../../crates/harn-vm/src/llm/agent_observe.rs) | Every `provider_call_request` / `provider_call_response` / `message` event is scrubbed before it is appended to `llm_transcript.jsonl` and the `agent.transcript.llm` topic. |
| `Receipt` envelopes | [`Receipt::redact_in_place`](../../crates/harn-vm/src/receipts/mod.rs) | Hosts wrap their `ReceiptSink` with `RedactingReceiptSink` to redact every persisted receipt without touching individual receipt-builder call sites. Schema-required identifiers (`id`, `trace_id`, `persona`, `status`, digests) remain stable for query and replay. |
| `LogEvent` payloads | [`LogEvent::redact_in_place`](../../crates/harn-vm/src/event_log/mod.rs) | Headers are scrubbed in-place; the JSON payload is walked recursively. Backends are intentionally unaware of redaction so emitters that need it call this before appending. |
| Observability events | [`std/observability`](./stdlib/observability.md) `processors: [obs().Processor.redaction]` | The processor walks the enriched event once before routing, so every configured backend receives the same scrubbed payload. |
| Portal transcript projection | [`portal::transcript`](../../crates/harn-cli/src/commands/portal/transcript.rs) | Defense in depth: even transcripts written before the unified policy landed are redacted at portal read time. |
| Workflow artifacts | [`ArtifactRecord::redact_in_place`](../../crates/harn-vm/src/orchestration/artifacts.rs) | `text` is run through the secret-pattern scanner; `data` and `metadata` go through the JSON walk. |
| Session bundles | [`harn_vm::session_bundle`](../../crates/harn-vm/src/session_bundle.rs) | Sanitized exports walk the whole bundle and emit a redaction manifest; replay-only exports additionally withhold prompt and tool payload fields. Import and validation reject high-confidence secret markers unless explicitly allowed. |
| Connector inbound headers | [`HeaderRedactionPolicy` (alias for `RedactionPolicy`)](../../crates/harn-vm/src/triggers/event.rs) | Each connector strips inbound HTTP headers before they reach the durable inbox. |
| HTTP egress diagnostics | [`crate::egress`](../../crates/harn-vm/src/egress.rs) | URLs reported in `EgressBlocked` errors and `http_mock` calls are redacted via the same policy. |

## Host extension points

The default policy is not the only policy. Hosts (Burin Code, the
Harn portal, third-party orchestrators) may want to:

- **Mark an additional header as safe.** A bespoke webhook can opt
  back into raw delivery by adding it to the safe-list:

  ```rust
  use harn_vm::redact::{push_policy, RedactionPolicy};
  push_policy(RedactionPolicy::default().with_safe_header("X-My-Trace-Id"));
  ```

- **Mark an additional header as sensitive.** Host-explicit deny
  substrings *override* the built-in safe-list — that is how a host
  says "treat my own delivery header as sensitive even though Harn
  would normally keep it for debugging":

  ```rust
  push_policy(RedactionPolicy::default().with_deny_header_substring("delivery"));
  ```

- **Add an internal field name to the redaction set.**
  `with_extra_field("internal_audit_token")` ensures that any JSON
  walk through receipts, event payloads, or artifacts replaces the
  field's value with `[redacted]` regardless of how the value was
  serialized.

- **Add an internal URL parameter.** `with_extra_url_param("session")`
  scrubs `?session=…` everywhere `redact_url` runs.

- **Disable the heuristic free-form scanner.** Performance-sensitive
  paths that have already been audited can opt out with
  `RedactionPolicy::default().disable_string_scan()`.

The active policy is a thread-local stack mirroring the existing
approval / capability policy stacks in
[`crate::orchestration::policy`](../../crates/harn-vm/src/orchestration/policy/mod.rs).
Push a host policy at orchestrator startup with `push_policy(...)`,
or scope it to a single execution with the RAII `PolicyGuard`. The
stack is cleared by `harn_vm::reset_thread_local_state()`, so test
runs that share a thread cannot leak overrides into each other.

## What we do not redact

- **Schema-required identifier fields on receipts** — `id`, `trace_id`,
  `persona`, `status`, `started_at`, `cost_usd`, and digests are part
  of the receipt envelope contract. Replay and reconciliation expect
  them to round-trip stable.
- **Connector metric counters** — the `ConnectorMetricsSnapshot` is
  numeric and carries no caller-supplied data.
- **Public artifact metadata fields with non-sensitive names** —
  `kind`, `title`, `created_at`, `lineage` carry routing/replay
  information rather than secrets.

If a host's product domain has *additional* fields it considers
sensitive, the right answer is `with_extra_field` / `with_extra_url_param`,
not editing the default deny list. The default list is the floor, not
the ceiling.

## Verification

End-to-end fixture coverage lives in
[`crates/harn-vm/tests/redaction_fixtures.rs`](../../crates/harn-vm/tests/redaction_fixtures.rs).
Each test stages a representative secret (Stripe, GitHub PAT, AWS
access key, Bearer token, URL with userinfo) through one persistence
surface and asserts the secret never appears in the rendered JSON.
Adding a new persistence surface that should respect the policy means
adding a fixture there, not a new redaction helper.
