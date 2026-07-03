# A2A RFC: ambient reminder injection for peer agents

**Upstream repo:** [a2aproject/A2A][a2a]
**Status:** Draft (not yet filed upstream). Revised 2026-07-03 to A2A
v1.0 conventions: `a2a.proto` is now the normative source of truth,
the proposed operation uses PascalCase (`InjectTaskReminder`),
streamed reminder lifecycle uses a member-typed `TaskReminderEvent`
on the `SubscribeToTask` stream (v1.0 removed `kind`-discriminated
notifications), and enum values use SCREAMING_SNAKE_CASE ProtoJSON
strings. v1.0's removal of the `Message.kind` discriminator directly
affects Option A below (see that section).
**Authors:** Burin Labs
**Reference impl:** `harn-serve` A2A adapter
([`crates/harn-serve/src/adapters/a2a/`][a2a-dir]) + typed
`SystemReminder` envelope
([`crates/harn-vm/src/llm/helpers/transcript.rs`][reminder-rs]).
**Sibling discussions:** [A2A #1857 — idempotency on the send
operation (`SendMessage` in v1.0)][a2a-1857] covers a different
concern (request idempotency); reminder injection is still open.

[a2a]: https://github.com/a2aproject/A2A
[a2a-1857]: https://github.com/a2aproject/A2A/discussions/1857
[a2a-dir]: https://github.com/burin-labs/harn/tree/main/crates/harn-serve/src/adapters/a2a
[reminder-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-vm/src/llm/helpers/transcript.rs

## Problem statement

A2A models agent-to-agent communication around `Message` (immediate
exchange) and `Task` (long-running work with streaming updates). Both
shapes assume the originator wants to **add content the peer treats as
turn input** — either a user-role prompt or an artifact attached to a
task.

There is no first-class shape for **ambient context injection**: a
short-lived, non-user-authored signal an A2A caller wants the peer to
factor into its next turn without claiming the caller said it. The
need is the same as the ACP `session/inject_reminder` case (see the
[sibling RFC](./acp-session-inject-reminder.md)), but A2A's transport
and message model are different enough to warrant a separate
discussion.

Concrete A2A scenarios:

- A coordinator agent telling a worker "the upstream PR you depend on
  just merged; rebase before retrying" — without that string ending
  up in the worker's user transcript.
- A monitoring agent injecting "your last action exceeded the configured
  cost budget" mid-task as a steering nudge.
- A workflow agent forwarding a host's file-watcher event to a
  delegated worker.

Today implementors invent one of three workarounds:

1. Send a `Message` with role `ROLE_USER` and a synthetic "System
   reminder:" prefix. Pollutes the worker's user transcript.
2. Attach a custom artifact to the running task. Wrong primitive —
   artifacts are outputs, not turn-scoped context.
3. Tunnel reminders through `metadata` on a `SendMessage` call.
   Works today but every adopter invents their own schema.

## Design decision: two shapes considered

Two shapes were considered for upstream proposal:

### Option A: a new `Message` role or member

A2A v0.x carried a `kind` discriminator on `Message` that this RFC
originally proposed extending with a `Reminder` value. **v1.0 removed
`kind` discriminators entirely** in favor of member-based
polymorphism, so that exact shape no longer exists. The nearest v1.0
equivalents are:

- **A new `Role` enum value** (e.g. `ROLE_REMINDER` alongside
  `ROLE_USER` / `ROLE_AGENT`). Reminder messages ride the same
  transports as ordinary messages.
- **A new member on `Message`** (a `reminder` submessage detected by
  member presence, the v1.0 polymorphism convention).

**Pros:** little or no new RPC surface; clients that don't recognize
the role/member can fall back to treating the message as opaque.

**Cons:** `Role` is a role discriminator, and adding a non-role value
muddies the existing semantic; implementations that filter messages
by role (most of them) silently drop reminders. A new `Message`
member is cleaner than the old `kind` hack but still forces every
message consumer to learn reminder lifecycle fields (TTL, dedupe,
propagation) that don't belong on a `Message`. **v1.0's removal of
the `kind` discriminator removes the one cheap argument this option
had** (piggybacking on an existing discriminator) and strengthens the
case for a dedicated operation.

### Option B: `InjectTaskReminder` JSON-RPC method (recommended)

A dedicated operation, sibling to `SendMessage` and the `Task`
family. Reminders are a distinct kind of payload with a distinct
lifecycle, so they get a distinct RPC. In v1.0 JSON-RPC bindings this
is invoked as `"method": "InjectTaskReminder"`.

**Pros:** keeps `Message` purely about role-tagged content; reminder
lifecycle (TTL, dedupe, propagation) doesn't pollute the message
schema; capability negotiation is unambiguous (peers either implement
the method or they don't).

**Cons:** one more method on the wire surface; peers that want to
support reminders must add a handler rather than just recognizing a
new role/member.

**Recommendation: Option B.** The lifecycle fields (TTL, dedupe,
propagate) don't belong on a `Message`, and v1.0's removal of the
`kind` discriminator means Option A no longer even saves a new
discriminator. A dedicated operation localizes the impact.

The remainder of this RFC specifies Option B; Option A is documented
above so the upstream discussion can revisit if maintainers prefer it.

## Proposed wire format

### `InjectTaskReminder` (caller → peer)

Since v1.0 `a2a.proto` is normative, we sketch the payload as proto.
Enum-valued fields follow the SCREAMING_SNAKE_CASE, type-prefixed
convention and render as those names in ProtoJSON:

```proto
enum ReminderPropagation {
  REMINDER_PROPAGATION_UNSPECIFIED = 0;
  REMINDER_PROPAGATION_NONE = 1;
  REMINDER_PROPAGATION_SESSION = 2;
}

enum ReminderRoleHint {
  REMINDER_ROLE_HINT_UNSPECIFIED = 0;
  REMINDER_ROLE_HINT_SYSTEM = 1;
  REMINDER_ROLE_HINT_DEVELOPER = 2;
}

message Reminder {
  string id = 1;
  string body = 2;
  repeated string tags = 3;
  optional string dedupe_key = 4;
  optional uint32 ttl_turns = 5;
  bool preserve_on_compact = 6;
  ReminderPropagation propagate = 7;
  ReminderRoleHint role_hint = 8;
  optional string source = 9;
  optional uint32 fired_at_turn = 10;
  // Delivery timing; mirrors the ACP sibling's mode taxonomy
  // (interrupt_immediate / finish_step / wait_for_completion).
  optional string mode = 11;
}
```

JSON-RPC request (ProtoJSON params, lowerCamelCase fields):

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "method": "InjectTaskReminder",
  "params": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "reminder": {
      "id": "reminder-019abf6b-7d51-7c1d-bb02-...",
      "body": "Upstream dependency PR landed; rebase before the next tool call.",
      "tags": ["upstream", "rebase"],
      "dedupeKey": "upstream:pr-1234",
      "ttlTurns": 2,
      "preserveOnCompact": true,
      "propagate": "REMINDER_PROPAGATION_SESSION",
      "roleHint": "REMINDER_ROLE_HINT_SYSTEM",
      "source": "coordinator-agent",
      "firedAtTurn": 4,
      "mode": "finish_step"
    },
    "metadata": {
      "harn.reminder.origin": "workflow-coordinator"
    }
  }
}
```

JSON-RPC response:

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "result": {
    "reminderId": "reminder-019abf6b-7d51-7c1d-bb02-...",
    "dedupedCount": 1,
    "acceptedAt": "2026-04-30T12:34:56.789Z"
  }
}
```

Error responses reuse A2A's v1.0 JSON-RPC error taxonomy:

| Code | Meaning |
|---|---|
| `-32602` | Malformed `reminder` payload (missing `body`, unknown enum value, etc.). |
| `-32001` | `TaskNotFoundError` — unknown `taskId`. |
| `-32601` | Peer does not implement `InjectTaskReminder` (method not found / capability missing). |

Peers that gate reminders behind a required extension can instead
reject with `-32008` (`ExtensionSupportRequiredError`).

### Streaming event: `TaskReminderEvent`

v1.0 removed `kind`-discriminated notifications: the `SubscribeToTask`
stream carries a `StreamResponse` whose members discriminate by JSON
member name (`task`, `message`, `statusUpdate`, `artifactUpdate`).
Reminder emission introduces a new event message type,
`TaskReminderEvent`, surfaced as a new `StreamResponse` member
(`reminderUpdate`) alongside `TaskStatusUpdateEvent` /
`TaskArtifactUpdateEvent` — the same pattern `artifactUpdate` already
follows. Delivered as the SSE `data` of a JSON-RPC response on the
`SubscribeToTask` stream:

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "result": {
    "reminderUpdate": {
      "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
      "contextId": "ctx-019abf6b-...",
      "reminder": {
        "id": "reminder-019abf6b-7d51-7c1d-bb02-...",
        "body": "Upstream dependency PR landed; rebase before the next tool call.",
        "tags": ["upstream", "rebase"],
        "dedupeKey": "upstream:pr-1234",
        "source": "coordinator-agent",
        "firedAtTurn": 5
      },
      "metadata": {}
    }
  }
}
```

The rest of the lifecycle rides the same `TaskReminderEvent` with a
`reason` member rather than separate event types: `REMINDER_DEDUPED`
(a newer reminder displaced one or more older ones sharing the same
`dedupeKey`) and `REMINDER_EXPIRED` (TTL reached zero or compaction
dropped it).

### Agent card capability

Peers that implement reminder injection advertise it on their agent
card. v1.0's `AgentCapabilities` carries `streaming`,
`pushNotifications`, `extensions`, and `extendedAgentCard`; this
proposal adds a `reminders` capability alongside them:

```json
{
  "name": "rebase-worker",
  "url": "https://example.com/.well-known/a2a-agent",
  "capabilities": {
    "streaming": true,
    "reminders": {
      "inject": true,
      "emit": true,
      "propagate": [
        "REMINDER_PROPAGATION_SESSION",
        "REMINDER_PROPAGATION_NONE"
      ],
      "roleHints": [
        "REMINDER_ROLE_HINT_SYSTEM",
        "REMINDER_ROLE_HINT_DEVELOPER"
      ]
    }
  }
}
```

Callers MUST treat absent `capabilities.reminders` as "not
supported" and fall back to either suppressing the reminder or
attaching it as task metadata (the current `_meta`-style workaround).
Alternatively, reminders could be declared as a versioned A2A
extension (see open questions) rather than a new core
`AgentCapabilities` field.

## Compatibility and migration

### From the current `_meta` envelope

Harn-as-A2A-peer currently:

- Accepts reminders smuggled through `SendMessage`'s `metadata` map
  under `metadata.harn.reminder`.
- Decorates outbound `SubscribeToTask` stream events with
  `metadata.harn.reminder` when an internal reminder fires during
  task execution.

Migration when the standardized method lands:

1. Implement `InjectTaskReminder` as the canonical inbound path.
   Keep `metadata.harn.reminder` reads as a fall-back for one A2A
   minor version.
2. Replace `metadata.harn.reminder` decoration with the standardized
   `TaskReminderEvent` stream event.
3. Add `capabilities.reminders` to the published agent card.
4. Regenerate `spec/protocol-artifacts/`
   (`make gen-protocol-artifacts`).

### For other A2A peers adopting this proposal

Peers that don't model reminders internally can satisfy
`InjectTaskReminder` by injecting the body as a synthetic
system-role context block on the next turn, with TTL=1 and no dedupe.
That's still strictly better than the synthetic-user-message
workaround.

## Reference implementation status

| Surface | Status | Notes |
|---|---|---|
| `metadata.harn.reminder` inbound handling | Shipping (v0.8.x) | Harn A2A adapter accepts reminders via `SendMessage` metadata today. |
| Outbound reminder emission on streaming tasks | Reference impl tracked in [#1828](https://github.com/burin-labs/harn/issues/1828) | Emitted as `metadata.harn.reminder` on existing SSE updates until upstream lands. |
| Typed `SystemReminder` lifecycle envelope | Shipping | Shared with ACP and MCP adapters. |
| Agent card `capabilities.reminders` advertisement | Pending upstream schema | Currently advertised under `capabilities._meta.harn.reminders`. |

The canonical lifecycle struct ([`SystemReminder`][reminder-rs]) is
shared verbatim with the [ACP RFC](./acp-session-inject-reminder.md);
field names round-trip through the A2A JSON shape with conventional
camelCase translation.

## Open questions for upstream maintainers

1. **Operation placement.** Does `InjectTaskReminder` belong on the
   `Task` family (as proposed), the message family (`SendMessage` /
   `SendStreamingMessage`), or a new session-oriented family that
   better mirrors ACP? A2A's evolution toward richer sessions argues
   for a session surface; the current task-centric surface argues for
   keeping it with the `Task` operations.
2. **Event shape.** We model emission as a new `TaskReminderEvent`
   member (`reminderUpdate`) on `StreamResponse`, mirroring how
   `artifactUpdate` carries `TaskArtifactUpdateEvent`. Maintainers may
   instead prefer folding reminders into `TaskStatusUpdateEvent.metadata`
   to avoid growing the `StreamResponse` oneof.
3. **Lifecycle visibility.** Should the `REMINDER_DEDUPED` and
   `REMINDER_EXPIRED` reasons ship in the first iteration, or only
   plain emission? Our experience with the ACP equivalent says hosts
   need dedupe visibility to render non-flickering UIs.
4. **Cross-task propagation.** A2A workflows often spawn sub-tasks.
   Should a `REMINDER_PROPAGATION_ALL` value carry the reminder into
   sub-tasks automatically, or require the parent to re-inject? The
   simpler session-only default keeps the v1 scope tight.
5. **Push notification interaction.** A2A push notifications already
   exist; should reminders piggyback on them or stay on the SSE
   stream? Our reference impl uses SSE only — push payloads weren't
   designed for short-lived turn-scoped context.
6. **Relationship to `SendMessage`.** When an A2A peer is being
   driven through `SendMessage` (not a task), is there a need for a
   message-scoped reminder inject too? We've found tasks cover the
   compelling use cases; bare `SendMessage` flows are short enough
   that the inject window is small.
7. **Ship as a versioned extension?** v1.0 enhanced the extension
   mechanism with per-extension versioning and `required` requirement
   declarations. Maintainers may prefer reminder injection to incubate
   as a declared A2A extension (advertised via
   `AgentCapabilities.extensions` with its own version) before any
   core operation, `Message`, or `StreamResponse` additions, so the
   surface can stabilize out of band.

## References

- [A2A #1857 — idempotency on the send operation (`SendMessage` in
  v1.0)][a2a-1857] (separate concern; not a substitute for reminder
  injection)
- [Sibling ACP RFC: `session/inject_reminder`](./acp-session-inject-reminder.md)
- [Sibling MCP RFC: `notifications/reminder`](./mcp-notifications-reminder.md)
- [System reminders user guide](../system-reminders.md)
- [`SystemReminder` struct][reminder-rs]
