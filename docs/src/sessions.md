# Sessions

A **session** is a first-class VM resource that owns three things for a
given conversational agent run:

1. Its **transcript history** (`messages`, `events`, `summary`, …).
2. The **closure subscribers** registered against it via
   `agent_subscribe(session_id, cb)`.
3. Its **lifecycle** — create, reset, fork, trim, compact, close.

Sessions replace the old `transcript_policy` config pattern. Lifecycle
used to be a side effect of dict fields (`mode: "reset"`, `mode: "fork"`
quietly surgerying state on stage entry); it is now expressed by
explicit, imperative builtins. Unknown inputs are hard errors.

## Quick tour

```harn
pipeline main(task) {
  // Open (or resume) a session. `nil` mints a UUIDv7.
  let s = agent_session_open()

  // Seed the conversation.
  agent_session_inject(s, {role: "user", content: "Hello!"})

  // Run an agent loop against the session — prior messages are
  // automatically loaded as prefix, the final transcript is persisted
  // back under `s`.
  let first = agent_loop("continue the greeting", nil, {
    session_id: s,
    provider: "mock",
  })

  // A second call sees `first`'s assistant reply as prior history.
  let second = agent_loop("what do you remember?", nil, {
    session_id: s,
    provider: "mock",
  })

  // Fork to explore a counterfactual without touching `s`.
  let branch = agent_session_fork(s)
  agent_session_inject(branch, {role: "user", content: "what if …"})

  // Or branch from a scrubber-rebuilt prefix.
  let replay_branch = agent_session_fork_at(s, 1)
  let ancestry = agent_session_ancestry(replay_branch)
  assert(ancestry["root_id"] == s, "fork ancestry resolves back to the root session")

  // Release a session immediately.
  agent_session_close(branch)
  agent_session_close(replay_branch)
}
```

If you don't pass `session_id` to `agent_loop`, the loop mints an
anonymous id internally and does NOT persist anything. That preserves
the "one-shot" call shape.

## Builtins

| Function | Returns | Notes |
|---|---|---|
| `agent_session_open(id?: string)` | `string` | Idempotent. `nil` mints a UUIDv7. |
| `agent_session_exists(id)` | `bool` | Safe on unknown ids. |
| `agent_session_current_id()` | `string` or `nil` | Returns the innermost active session id for the current thread, or `nil` outside any active session. |
| `agent_session_length(id)` | `int` | Message count. Errors if `id` doesn't exist. |
| `agent_session_snapshot(id)` | `dict` or `nil` | Read-only transcript snapshot plus `length`, `created_at`, `system_prompt`, `tool_format`, `parent_id`, `child_ids`, and `branched_at_event_index`. |
| `agent_session_ancestry(id)` | `dict` or `nil` | Returns `{parent_id, child_ids, root_id}` for the in-VM session graph. |
| `agent_session_reset(id)` | `nil` | Wipes history; preserves id and subscribers. |
| `agent_session_fork(src, dst?)` | `string` | Copies transcript, sets parent/child lineage, and does NOT copy subscribers. |
| `agent_session_fork_at(src, keep_first, dst?)` | `string` | Forks then keeps only the first `keep_first` messages on the child. Records `branched_at_event_index`. |
| `agent_session_trim(id, keep_last)` | `int` | Retains last `keep_last` messages. Returns kept count. |
| `agent_session_compact(id, opts)` | `int` | Runs the LLM/truncate/observation-mask/custom compactor. Unknown keys in `opts` error. |
| `agent_session_inject(id, message)` | `nil` | Appends a `{role, content, …}` message. Missing `role` errors. |
| `agent_session_seed_from_jsonl(jsonl_path, opts?)` | `dict` | Creates a new session from a replayable `llm_transcript.jsonl` sidecar. |
| `agent_session_close(id, status?)` | `nil` | Evicts immediately and records an `agent_session_closed` event. `status` may be a string reason or a dict such as `{reason: "timeout"}`. |

### `agent_session_compact` options

Accepts any subset of these keys; anything else is a hard error:

- `keep_last` (int, default 12)
- `token_threshold` (int)
- `tool_output_max_chars` (int)
- `compact_strategy` (`"llm" | "truncate" | "observation_mask" | "custom"`)
- `hard_limit_tokens` (int)
- `hard_limit_strategy` (same values as above)
- `custom_compactor` (closure)
- `mask_callback` (closure)
- `compress_callback` (closure)

Use `compact_strategy: "custom"` with `custom_compactor` to replace the
compaction scheme completely. `mask_callback` and `compress_callback` customize
the built-in observation-mask path without changing the rest of the session
lifecycle.

### `agent_session_seed_from_jsonl`

`agent_session_seed_from_jsonl(path, opts?)` imports prompt-visible history
from an LLM transcript sidecar and creates a new session preloaded with those
messages:

```harn
let seeded = agent_session_seed_from_jsonl(
  ".harn-runs/audit/agent-llm/llm_transcript.jsonl",
  {truncate_to_last: 40, rename_session: "audit-recovery"},
)

if seeded.ok {
  agent_loop("Continue from the failed release step.", nil, {
    session_id: seeded.session_id,
    provider: seeded.provider,
  })
}
```

Options:

- `truncate_to_last` (int): keep only the last N reconstructed messages.
- `drop_tool_calls` (bool, default `false`): remove assistant tool-call payloads
  and tool-result messages, leaving only user/assistant text.
- `rename_session` (string): requested id for the new session; errors if it
  already exists.
- `validate` (bool, default `true`): require an exact replay source. Sidecars
  that contain only provider responses are rejected because user/tool-result
  turns cannot be reconstructed from them.
- `provider` / `model` (string): optional guardrails. When set, validation
  checks the transcript's recorded request provider/model before seeding.

The result shape is `{ok, session_id?, turns_loaded?, messages_loaded?,
source_records?, source_format?, partial?, truncated?, provider?, model?,
tool_format?, error?}`. `source_format` is `message_events`,
`request_snapshots`, or `provider_responses_only`; the last is available only
with `validate: false` and is assistant-response best effort, not prefix-cache
equivalent replay.

## Storage model

Sessions live in a per-thread `HashMap<String, SessionState>` in
`crate::agent_sessions`. Thread-local is correct because `VmValue`
wraps `Rc` and the agent loop runs on a pinned tokio `LocalSet` task.

An LRU cap (default 128 sessions per VM) evicts the least-recently
accessed session when a new one is opened over the cap.
`agent_session_close` evicts immediately regardless of the cap. When passed a
reason string or status dict, the close reason is emitted to the agent event
stream before the session is removed.

## Subscribers

`agent_subscribe(id, closure)` appends `closure` to the session's
`subscribers` list. The agent loop fires `turn_end` (and other)
events through every subscriber for that session id. Subscribers are
not copied by `agent_session_fork` — a fork is a conversation branch,
not an event fanout.

Inside those callbacks, `agent_session_current_id()` resolves to the
session currently being driven by the agent loop. Outside any active
session, it returns `nil`.

## Lineage

Forks now populate a small in-memory ancestry graph:

- `agent_session_fork(src, dst?)` sets the child's `parent_id` and appends the child id to the parent's `child_ids`.
- `agent_session_fork_at(src, keep_first, dst?)` does the same and also records `branched_at_event_index` on the child snapshot.
- `agent_session_ancestry(id)` walks parent links up to the reachable root and returns `{parent_id, child_ids, root_id}`.

This lineage stays VM-local. It is meant for host UIs and replay tools
that want to render branching conversation trees without re-deriving
parentage from workflow state or external logs.

## Durable orchestrator sessions

The VM-local session builtins above own transcript state. Long-running
orchestrator deployments often need a second, transport-facing primitive:
an opaque bearer session that records who is calling the service and when
the token expires. That store lives in `harn_vm::sessions` and is backed
by the same crash-safe EventLog as trigger queues and orchestrator state.
The raw bearer token is returned only from `create`; the EventLog stores a
one-way token handle so logs and backups are not bearer credentials.

The durable record shape is:

| Field | Type | Meaning |
|---|---|---|
| `id` | string | Opaque bearer token. It is present in API results, not persisted in raw form. |
| `principal` | string | Stable user, tenant, service, or client identity. |
| `created_at` | RFC 3339 timestamp | Creation time. |
| `last_seen_at` | RFC 3339 timestamp | Last successful touch/authentication time. |
| `expires_at` | RFC 3339 timestamp | Absolute expiration time. |
| `attributes` | JSON object | Small caller-owned metadata such as role, tenant, or client labels. |

The public Rust API is `harn_vm::sessions::{create, get, touch,
expire}` plus the equivalent `SessionStore` methods. `get` and `touch`
return `None` for unknown or expired sessions, so HTTP middleware can map
that result directly to `401`. Caller-supplied session ids must be high
entropy; omitting `id` lets Harn generate a token suitable for bearer
auth.

```rust,ignore
use std::collections::BTreeMap;
use std::sync::Arc;

use time::{Duration, OffsetDateTime};

let store = Arc::new(harn_vm::SessionStore::new(event_log.clone()));
let attributes = BTreeMap::from([
    ("role".to_string(), serde_json::json!("operator")),
    ("tenant".to_string(), serde_json::json!("acme")),
]);
let session = harn_vm::sessions::create(
    &store,
    harn_vm::CreateSession {
        id: None,
        principal: "user_123".to_string(),
        created_at: None,
        expires_at: OffsetDateTime::now_utc() + Duration::days(7),
        attributes,
    },
)
.await?;

// Send this to clients as `Authorization: Bearer <id>`.
let bearer = session.id;
```

### Middleware integration

An HTTP service can compose authentication by trying its local API-key
or OAuth verifier first, then falling back to a durable Harn session id:

```rust,ignore
struct AuthPrincipal(String);
struct AuthSessionId(String);

let token = authorization_header
    .strip_prefix("Bearer ")
    .ok_or(AuthError::Unauthorized)?;

let Some(session) = harn_vm::sessions::touch(
    &store,
    harn_vm::TouchSession::new(token, OffsetDateTime::now_utc()),
)
.await?
else {
    return Err(AuthError::Unauthorized);
};

request.extensions_mut().insert(AuthPrincipal(session.principal));
request.extensions_mut().insert(AuthSessionId(session.id));
```

`harn orchestrator serve` uses the same composition point internally:
listener bearer auth accepts configured static API keys first and then
checks the attached `SessionStore`. A successful durable-session auth
touches `last_seen_at`; expired sessions are rejected without extending
their lifetime. Call `expire` to revoke a session explicitly.

## Interaction with workflows

Workflow stages pick up a session id from
`model_policy.session_id` on the node; if unset, each stage mints a
stable stage-scoped id. Two stages sharing a `session_id` share their
transcript automatically through the session store — no explicit
threading or policy dict required.

To branch a stage's conversation before running it, call
`agent_session_fork` in the pipeline before `workflow_execute` and
wire the fork id into the relevant node's `model_policy.session_id`.

## Fail-loud

Unknown option keys on `agent_session_compact`, a missing `role` on
`agent_session_inject`, a negative `keep_last`, and any of the
lifecycle verbs (`reset`, `fork`, `fork_at`, `close`, `trim`,
`inject`, `length`, `compact`) called against an unknown id all raise
a `VmError::Thrown(string)`. `exists`, `open`, `snapshot`, and
`ancestry` are the only calls that tolerate unknown ids by design.
