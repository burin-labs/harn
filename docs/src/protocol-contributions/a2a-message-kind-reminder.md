# A2A RFC: ambient reminder injection for peer agents

**Upstream repo:** [a2aproject/A2A][a2a]
**Status:** Draft (not yet filed upstream).
**Authors:** Burin Labs
**Reference impl:** `harn-serve` A2A adapter
([`crates/harn-serve/src/adapters/a2a/`][a2a-dir]) + typed
`SystemReminder` envelope
([`crates/harn-vm/src/llm/helpers/transcript.rs`][reminder-rs]).
**Sibling discussions:** [A2A #1857 — idempotency on `tasks/send`][a2a-1857]
covers a different concern (request idempotency); reminder injection
is still open.

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

1. Send a `Message` of role `user` with a synthetic "System reminder:"
   prefix. Pollutes the worker's user transcript.
2. Attach a custom artifact to the running task. Wrong primitive —
   artifacts are outputs, not turn-scoped context.
3. Tunnel reminders through `metadata` on a `tasks/send` call. Works
   today but every adopter invents their own schema.

## Design decision: two shapes considered

Two shapes were considered for upstream proposal:

### Option A: `Message.kind: "Reminder"`

Add `Reminder` to the existing `Message.kind` discriminator alongside
`user` and `agent`. Reminder messages ride the same transports as
ordinary messages.

**Pros:** zero new RPC surface; clients that don't recognize the kind
fall back to treating the message as opaque.

**Cons:** `Message.kind` is currently a role discriminator. Adding a
non-role value muddies the existing semantic. Implementations that
filter messages by role (most of them) silently drop reminders.

### Option B: `tasks/inject_reminder` JSON-RPC method (recommended)

A dedicated JSON-RPC method, sibling to `tasks/send` and the existing
`message/*` family. Reminders are a distinct kind of payload with a
distinct lifecycle, so they get a distinct RPC.

**Pros:** keeps `Message` purely about role-tagged content; reminder
lifecycle (TTL, dedupe, propagation) doesn't pollute the message
schema; capability negotiation is unambiguous (peers either implement
the method or they don't).

**Cons:** one more method on the wire surface; peers that want to
support reminders must add a handler rather than just recognizing a
new kind.

**Recommendation: Option B.** The lifecycle fields (TTL, dedupe,
propagate) don't belong on a `Message` and trying to attach them
there forces every message consumer to learn about them. A dedicated
method localizes the impact.

The remainder of this RFC specifies Option B; Option A is documented
above so the upstream discussion can revisit if maintainers prefer it.

## Proposed wire format

### `tasks/inject_reminder` (caller → peer)

JSON-RPC request:

```json
{
  "jsonrpc": "2.0",
  "id": "req-019abf6b-...",
  "method": "tasks/inject_reminder",
  "params": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
    "reminder": {
      "id": "reminder-019abf6b-7d51-7c1d-bb02-...",
      "body": "Upstream dependency PR landed; rebase before the next tool call.",
      "tags": ["upstream", "rebase"],
      "dedupeKey": "upstream:pr-1234",
      "ttlTurns": 2,
      "preserveOnCompact": true,
      "propagate": "session",
      "roleHint": "system",
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

Error responses follow A2A's existing JSON-RPC error envelope:

| Code | Meaning |
|---|---|
| `-32602` | Malformed `reminder` payload (missing `body`, unknown enum value, etc.). |
| `-32004` | Unknown `taskId`. |
| `-32601` | Peer does not implement `tasks/inject_reminder` (i.e., capability missing). |

### Streaming notification: `tasks/reminderEmitted`

Mirrors the `tasks/artifact_update` notification pattern. Sent on the
existing `tasks/sendSubscribe` SSE channel:

```json
{
  "jsonrpc": "2.0",
  "method": "tasks/reminderEmitted",
  "params": {
    "taskId": "task-019abf6b-7d51-7c1d-bb02-...",
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
```

Two sibling notifications cover the rest of the lifecycle:
`tasks/reminderDeduped` (newer reminder displaced one or more older
ones sharing the same `dedupeKey`) and `tasks/reminderExpired` (TTL
reached zero or compaction dropped it).

### Agent card capability

Peers that implement reminder injection advertise it on their agent
card:

```json
{
  "name": "rebase-worker",
  "url": "https://example.com/.well-known/a2a-agent",
  "skills": ["rebase"],
  "capabilities": {
    "reminders": {
      "inject": true,
      "emit": true,
      "propagate": ["session", "none"],
      "roleHints": ["system", "developer"]
    }
  }
}
```

Callers MUST treat absent `capabilities.reminders` as "not
supported" and fall back to either suppressing the reminder or
attaching it as task metadata (the current `_meta`-style workaround).

## Compatibility and migration

### From the current `_meta` envelope

Harn-as-A2A-peer currently:

- Accepts reminders smuggled through `tasks/send`'s `metadata` map
  under `metadata.harn.reminder`.
- Decorates outbound `tasks/sendSubscribe` SSE events with
  `metadata.harn.reminder` when an internal reminder fires during
  task execution.

Migration when the standardized method lands:

1. Implement `tasks/inject_reminder` as the canonical inbound path.
   Keep `metadata.harn.reminder` reads as a fall-back for one A2A
   minor version.
2. Replace `metadata.harn.reminder` decoration with the standardized
   `tasks/reminderEmitted` notification.
3. Add `capabilities.reminders` to the published agent card.
4. Regenerate `spec/protocol-artifacts/`
   (`make gen-protocol-artifacts`).

### For other A2A peers adopting this proposal

Peers that don't model reminders internally can satisfy
`tasks/inject_reminder` by injecting the body as a synthetic
system-role context block on the next turn, with TTL=1 and no dedupe.
That's still strictly better than the synthetic-user-message
workaround.

## Reference implementation status

| Surface | Status | Notes |
|---|---|---|
| `metadata.harn.reminder` inbound handling | Shipping (v0.8.x) | Harn A2A adapter accepts reminders via `tasks/send` metadata today. |
| Outbound reminder emission on streaming tasks | Reference impl tracked in [#1828](https://github.com/burin-labs/harn/issues/1828) | Emitted as `metadata.harn.reminder` on existing SSE updates until upstream lands. |
| Typed `SystemReminder` lifecycle envelope | Shipping | Shared with ACP and MCP adapters. |
| Agent card `capabilities.reminders` advertisement | Pending upstream schema | Currently advertised under `capabilities._meta.harn.reminders`. |

The canonical lifecycle struct ([`SystemReminder`][reminder-rs]) is
shared verbatim with the [ACP RFC](./acp-session-inject-reminder.md);
field names round-trip through the A2A JSON shape with conventional
camelCase translation.

## Open questions for upstream maintainers

1. **Method placement.** Does `tasks/inject_reminder` belong on the
   `tasks/*` family (as proposed), the `message/*` family, or a new
   `session/*` family that better mirrors ACP? A2A's evolution toward
   richer sessions argues for `session/*`; the current task-centric
   surface argues for `tasks/*`.
2. **Notification naming.** `tasks/reminderEmitted` vs
   `tasks/reminder/emitted` (nested) vs `tasks/reminder_emitted`
   (snake_case to match A2A's existing `tasks/artifact_update`).
   We've used camelCase above; A2A's existing convention mixes both.
3. **Lifecycle visibility.** Should `reminderDeduped` and
   `reminderExpired` ship in the first iteration, or only
   `reminderEmitted`? Our experience with the ACP equivalent says
   hosts need dedupe visibility to render non-flickering UIs.
4. **Cross-task propagation.** A2A workflows often spawn sub-tasks.
   Should `propagate: "all"` carry the reminder into sub-tasks
   automatically, or require the parent to re-inject? The simpler
   "session-only" default keeps the v1 scope tight.
5. **Push notification interaction.** A2A push notifications already
   exist; should reminders piggyback on them or stay on the SSE
   stream? Our reference impl uses SSE only — push payloads weren't
   designed for short-lived turn-scoped context.
6. **Relationship to `message/send`.** When an A2A peer is being
   driven through `message/send` (not a task), is there a need for
   `messages/inject_reminder` too? We've found tasks cover the
   compelling use cases; bare `message/send` flows are short enough
   that the inject window is small.

## References

- [A2A #1857 — idempotency on `tasks/send`][a2a-1857] (separate
  concern; not a substitute for reminder injection)
- [Sibling ACP RFC: `session/inject_reminder`](./acp-session-inject-reminder.md)
- [Sibling MCP RFC: `notifications/reminder`](./mcp-notifications-reminder.md)
- [System reminders user guide](../system-reminders.md)
- [`SystemReminder` struct][reminder-rs]
