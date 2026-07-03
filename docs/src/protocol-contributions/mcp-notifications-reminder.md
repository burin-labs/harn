# MCP RFC: `notifications/reminder` server→host ambient context

**Upstream repo:** [modelcontextprotocol/specification][mcp-spec]
**Status:** Filed upstream 2026-07-03 as
[MCP discussion #3007](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3007),
awaiting first response.
**Authors:** Burin Labs
**Reference impl:** `harn-serve` MCP adapter
([`crates/harn-serve/src/adapters/mcp.rs`][mcp-rs]) + typed
`SystemReminder` envelope
([`crates/harn-vm/src/llm/helpers/transcript.rs`][reminder-rs]).
**Sibling discussions:** [MCP #2736 — budget caps on
`sampling/createMessage`][mcp-2736] covers a separate concern (request
budgeting); reminder notifications are still open.

[mcp-spec]: https://github.com/modelcontextprotocol/specification
[mcp-2736]: https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736
[mcp-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-serve/src/adapters/mcp.rs
[reminder-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-vm/src/llm/helpers/transcript.rs

## Problem statement

MCP servers expose tools, resources, and prompts to a host. The
existing server→host notification surface covers:

- `notifications/resources/updated` — a resource changed.
- `notifications/resources/list_changed` — the resource catalog
  changed.
- `notifications/tools/list_changed` — the tool catalog changed.
- `notifications/prompts/list_changed` — the prompt catalog changed.
- `notifications/progress` — long-running tool progress.

None of these model **ambient signals the server wants the host to
factor into the agent's next turn**. Concrete examples:

- A filesystem MCP server watches `src/` and notices the user just
  edited `lib.rs` in another editor. The host's running agent should
  re-read `lib.rs` before the next tool call. Today there's no way
  for the server to say so — it can only re-emit
  `notifications/resources/updated` for every dependent resource, and
  the host has to invent its own translation layer to turn that into
  a model-visible nudge.
- A build-watcher MCP server notices `cargo check` succeeded after
  the agent's last edit. The agent should know "the change you just
  made compiles" before deciding whether to keep editing.
- A test-watcher MCP server notices a previously failing test now
  passes. Same shape: a turn-scoped fact the model should see, that
  isn't a tool result and isn't a resource read.
- A CI/GitHub MCP server notices a PR review came in while the agent
  was working on a sibling file.

Today MCP server implementors smuggle these signals through one of:

1. `notifications/resources/updated` plus an out-of-band convention
   for "host should re-read and inject." Fragile.
2. A custom tool the agent must explicitly call ("check for ambient
   updates"). Loses the push semantics and burns a tool slot.
3. `notifications/message` or `notifications/progress` with the
   reminder text. Wrong primitive — those are about server logging
   and tool progress, not turn-scoped context.

## Proposed wire format

### `notifications/reminder` (server → host)

A new server-initiated notification, no `id` field (per the MCP
notification pattern):

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/reminder",
  "params": {
    "reminder": {
      "id": "0190abcd-2024-7c1d-bb02-3a0e8a44d7f0",
      "body": "src/lib.rs changed externally; re-read it before editing.",
      "tags": ["workspace", "file_changed"],
      "dedupeKey": "file_changed:src/lib.rs",
      "ttlTurns": 2,
      "preserveOnCompact": false,
      "propagate": "session",
      "roleHint": "system",
      "firedAtTurn": null
    },
    "_meta": {}
  }
}
```

JSON Schema sketch (matching the MCP spec repo's `.schema.json`
style):

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://modelcontextprotocol.io/schemas/notifications-reminder",
  "title": "ReminderNotification",
  "type": "object",
  "required": ["jsonrpc", "method", "params"],
  "properties": {
    "jsonrpc": { "const": "2.0" },
    "method": { "const": "notifications/reminder" },
    "params": {
      "type": "object",
      "required": ["reminder"],
      "properties": {
        "reminder": { "$ref": "#/$defs/ReminderSpec" },
        "_meta": { "type": "object" }
      }
    }
  },
  "$defs": {
    "ReminderSpec": {
      "type": "object",
      "required": ["id", "body"],
      "properties": {
        "id": { "type": "string", "description": "Stable reminder id (UUIDv7 recommended)." },
        "body": { "type": "string", "minLength": 1 },
        "tags": { "type": "array", "items": { "type": "string" } },
        "dedupeKey": { "type": "string" },
        "ttlTurns": { "type": "integer", "minimum": 1 },
        "preserveOnCompact": { "type": "boolean", "default": false },
        "propagate": { "enum": ["all", "session", "none"], "default": "session" },
        "roleHint": {
          "enum": ["system", "developer", "user_block", "ephemeral_cache"],
          "default": "system"
        },
        "firedAtTurn": { "type": ["integer", "null"] }
      }
    }
  }
}
```

### Server capability advertisement

MCP servers that can emit reminder notifications declare it during
`initialize`:

```json
{
  "capabilities": {
    "reminders": {
      "emit": true,
      "propagate": ["session", "none"],
      "roleHints": ["system", "developer"]
    }
  }
}
```

Hosts that receive `notifications/reminder` from a server that did
NOT declare `reminders.emit` SHOULD log and drop the notification
(per the existing MCP rule that unknown server messages are tolerated
but not acted on).

### Host-side rendering

The host is responsible for translating a reminder into the agent's
next model turn. Recommended behavior:

- Append the reminder body to a turn-scoped system or developer
  message (host's choice; `roleHint` is advisory).
- Honor `dedupeKey`: a newer reminder with the same key replaces
  older pending reminders that haven't yet fired.
- Honor `ttlTurns`: decrement after each rendered turn, drop at 0.
- Honor `preserveOnCompact`: if the host compacts the transcript,
  reminders flagged `true` survive the rewrite.
- `propagate` is a hint about sub-agent inheritance, which most MCP
  hosts won't model directly — safe to ignore in v1.

## Positioning vs file-watcher / build-watcher use cases

MCP already has `notifications/resources/updated`. Doesn't that cover
the file-watcher case?

No, and the distinction is important enough to call out:

- **`notifications/resources/updated`** is "the contents of resource
  URI X changed; if you have it cached, invalidate." It's a
  cache-invalidation signal aimed at host-side state, not model
  context. A host hearing it can re-read on demand, but the agent
  doesn't see anything unless the host invents a synthetic nudge.
- **`notifications/reminder`** is "tell the agent X happened, in
  natural language, on its next turn." The host doesn't need to
  invent the text; the server is the domain expert about why this is
  worth surfacing.

A well-behaved server would emit both: `resources/updated` so caches
invalidate, plus `notifications/reminder` so the agent learns about
the change in time to factor it into its next decision. The two
together replace today's "host invents text from a list of changed
URIs" pattern.

### Build-watcher example end-to-end

```text
[agent calls cargo check]
  -> [build-watcher MCP server runs, succeeds]
     -> notifications/reminder: "cargo check passed after your last edit."
        roleHint=system, ttlTurns=1, dedupeKey="cargo-check:status"

[agent's next turn]
  -> sees the reminder, decides whether to keep editing or move on
```

### Test-watcher example end-to-end

```text
[user edits test file in another editor]
  -> file-watcher resources/updated for tests/api_test.rs
     (host invalidates cache)
  -> test-watcher MCP server reruns affected suite
     -> notifications/reminder: "tests/api_test.rs now passes."
        roleHint=system, ttlTurns=2, dedupeKey="test:tests/api_test.rs"
```

## Compatibility and migration

### From the current `_meta` envelope

Harn-as-MCP-server (and Harn-as-MCP-host) currently:

- Accepts and emits reminders under
  `notifications/message` with a sentinel `_meta.harn.reminder`
  payload.
- Treats `_meta.harn.reminder` as the canonical lifecycle envelope,
  shared verbatim with the ACP and A2A adapters.

Migration when `notifications/reminder` lands upstream:

1. Implement `notifications/reminder` as the canonical inbound and
   outbound path.
2. Keep the `_meta.harn.reminder` reads on `notifications/message`
   as a back-compat fall-back for one MCP minor version.
3. Advertise `capabilities.reminders` on the server side; on the
   host side, key off the server's advertisement to decide whether
   to subscribe.
4. Regenerate `spec/protocol-artifacts/`
   (`make gen-protocol-artifacts`).

### For other MCP servers and hosts

Servers that don't model reminders today gain a simple opt-in path:
emit `notifications/reminder` whenever a watcher or background
process produces a fact the agent should know about. Hosts that don't
implement reminder rendering can ignore the notification cleanly —
unknown notifications are already required to be tolerated.

## Reference implementation status

| Surface | Status | Notes |
|---|---|---|
| `_meta.harn.reminder` outbound emission on MCP notifications | Reference impl tracked in [#1828](https://github.com/burin-labs/harn/issues/1828) | `crates/harn-serve/src/adapters/mcp.rs` |
| Typed `SystemReminder` lifecycle envelope | Shipping | Shared with ACP and A2A adapters. |
| Host-side rendering with provider capability dispatch | Shipping (v0.8.x) | See [System reminders > Capability-aware rendering](../system-reminders.md#capability-aware-rendering). |
| `capabilities.reminders` advertisement | Pending upstream schema | Currently under `capabilities._meta.harn.reminders`. |

The canonical lifecycle struct ([`SystemReminder`][reminder-rs]) is
shared verbatim with the ACP and A2A RFCs; the JSON Schema sketch
above is a direct projection of its Rust shape.

## Open questions for upstream maintainers

1. **Notification name.** `notifications/reminder` is the simplest
   form. Alternatives: `notifications/context/reminder`,
   `notifications/agent/reminder`. We've used the flat form for
   symmetry with `notifications/progress` and `notifications/message`.
2. **Scope.** Should reminders be tool-scoped (riding alongside an
   in-flight `tools/call`), session-scoped (the proposal above), or
   both? Our reference impl is session-scoped — the host decides
   which agent transcript the reminder applies to.
3. **Lifecycle visibility.** Should the host echo `reminderEmitted`
   / `reminderDeduped` / `reminderExpired` back to the server? MCP
   is typically asymmetric (servers push, hosts consume), so we've
   omitted echoes; a server that needs lifecycle visibility today
   includes its own correlation id in `_meta`.
4. **`roleHint` realism.** Same question as the ACP RFC: `user_block`
   and `ephemeral_cache` exist because Anthropic models materially
   prefer those shapes. Should MCP standardize these hints
   (acknowledging provider variance) or stay provider-agnostic with
   just `system`/`developer`?
5. **Relationship to `notifications/resources/updated`.** Should the
   spec explicitly recommend the pairing "emit both when the model
   needs to see the change"? Our experience is yes, but it may be
   out of scope for the wire-format spec and belong in a separate
   "best practices for ambient signals" document.
6. **Backpressure.** A misbehaving server could spam reminders. Hosts
   already have to rate-limit `notifications/message` and
   `notifications/progress`; should this RFC explicitly call out a
   recommended per-server reminder budget, or leave that to host
   policy?

## References

- [MCP #2736 — budget caps on `sampling/createMessage`][mcp-2736]
  (separate concern; not a substitute for reminder notifications)
- [Sibling ACP RFC: `session/inject_reminder`](./acp-session-inject-reminder.md)
- [Sibling A2A RFC: `tasks/inject_reminder`](./a2a-message-kind-reminder.md)
- [System reminders user guide](../system-reminders.md)
- [`SystemReminder` struct][reminder-rs]
