# ACP RFC: `session/inject_reminder` + `SessionUpdate::ReminderEmitted`

**Upstream repo:** [agentclientprotocol/agent-client-protocol][acp]
**Discussion:** [ACP #1224 — `session/remind` ambient system-role
context injection][acp-1224]
**Status:** Open upstream discussion; this document is the
schema-edit follow-up that #1224 anticipated.
**Authors:** Burin Labs
**Reference impl:** `harn-serve` ACP adapter
([`crates/harn-serve/src/adapters/acp/mod.rs`][acp-mod-rs]) +
typed `SystemReminder` envelope
([`crates/harn-vm/src/llm/helpers/transcript.rs`][reminder-rs]).

[acp]: https://github.com/agentclientprotocol/agent-client-protocol
[acp-1224]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224
[acp-mod-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-serve/src/adapters/acp/mod.rs
[reminder-rs]: https://github.com/burin-labs/harn/blob/main/crates/harn-vm/src/llm/helpers/transcript.rs

## Problem statement

ACP has two well-defined input channels for a session:

- `session/prompt` — the user's next turn, durably appended to the
  message list.
- `session/inject` (per [ACP #1220][acp-1220]) — mid-turn user-role
  steering that queues alongside the in-flight prompt.

Neither models **ambient context injection**: short-lived,
non-user-authored signals the host wants the model to see on its next
turn without claiming the user typed them. Concrete examples a host
might want to surface:

- A file the agent is editing changed externally while it was idle.
- A long-running build/test process finished and produced new output.
- The host's policy engine wants to remind the agent of a constraint
  (e.g., "this repo blocks force-pushes to main") before its next
  tool-use turn.
- Token-pressure or compaction-recap notes derived from transcript
  introspection.

Today every ACP host that wants this primitive invents its own
out-of-band channel — usually a synthetic user message prefixed with
"System reminder:". That conflates user input with system context,
pollutes the durable transcript with strings the user never said, and
makes audit/replay ambiguous about who said what.

[acp-1220]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1220

## Proposed wire format

### `session/inject_reminder` (client → agent)

A new JSON-RPC method on the agent side, sibling to `session/prompt`
and `session/inject`:

```typescript
/**
 * Inject an ambient system-role reminder into a running session.
 *
 * Reminders are short-lived context the host wants the model to see on
 * its next turn. They are NOT user input — they never appear in the
 * durable user-message stream — and they have an explicit lifecycle
 * (TTL, dedupe, propagation to sub-agents).
 *
 * Distinct from `session/inject`, which is user-role mid-turn steering.
 */
export interface SessionInjectReminderRequest {
  /** Target session. */
  sessionId: SessionId
  /** The reminder body the model will see on its next turn. */
  body: string
  /** Optional tags for host-side querying, clearing, and audit. */
  tags?: string[]
  /**
   * Optional stable key. A newer reminder with the same dedupeKey
   * replaces older pending reminders carrying that key.
   */
  dedupeKey?: string
  /**
   * Optional finite turn budget. After this many post-turn lifecycle
   * passes the reminder expires and is dropped. Omit for "persist
   * until cleared or compacted away."
   */
  ttlTurns?: number
  /**
   * When true, compaction copies this reminder forward into the
   * compacted transcript. Defaults to false.
   */
  preserveOnCompact?: boolean
  /**
   * Sub-agent propagation policy. Defaults to "session".
   */
  propagate?: "all" | "session" | "none"
  /**
   * Preferred rendering slot. The agent's provider-capability
   * dispatch makes the final decision; this is a hint, not a binding.
   */
  roleHint?: "system" | "developer" | "user_block" | "ephemeral_cache"
  /**
   * Delivery mode. Mirrors session/inject semantics:
   *   - "interrupt_immediate": drain at the next safe checkpoint,
   *     including pre-tool-dispatch (the pending tool batch is
   *     skipped when one arrives there).
   *   - "finish_step": drain at the next iteration boundary; the
   *     model renders the reminder on its next prompt.
   *   - "audit_only": drain at loop exit and append to the
   *     transcript audit. The model never sees these reminders —
   *     no further model call runs after loop exit. Use
   *     "finish_step" if the model must react before the agent
   *     terminates.
   */
  mode?: "interrupt_immediate" | "finish_step" | "audit_only"
  /** Extension envelope for protocol-extension fields. */
  _meta?: Record<string, unknown>
}

export interface SessionInjectReminderResponse {
  /** Server-assigned stable id, used in subsequent ReminderEmitted updates. */
  reminderId: string
  /**
   * Number of older pending reminders that were dropped because they
   * shared the supplied dedupeKey.
   */
  dedupedCount?: number
}
```

### `SessionUpdate::ReminderEmitted` (agent → client)

A new discriminator on the existing `session/update` notification's
`update` union, fired when the agent's reminder lifecycle commits a
reminder to the next model turn (whether the source was
`session/inject_reminder`, an internal provider, or a hook):

```typescript
export interface SessionUpdate_ReminderEmitted {
  sessionUpdate: "reminder_emitted"
  /** Stable id assigned to this reminder. */
  reminderId: string
  /** The reminder body that was rendered into the model turn. */
  body: string
  /** Optional tags. */
  tags?: string[]
  /** Optional dedupe key the reminder was registered under. */
  dedupeKey?: string
  /** Origin of this reminder (see source enum below). */
  source: "host" | "provider" | "hook" | "bridge" | "inherited"
  /** Provider id when source is "provider" or "hook". */
  providerId?: string
  /** Turn index when the reminder was injected (host counter). */
  firedAtTurn?: number
  /** Extension envelope. */
  _meta?: Record<string, unknown>
}
```

Two sibling lifecycle updates are recommended for completeness, mirroring
the events Harn already emits internally:

```typescript
export interface SessionUpdate_ReminderDeduped {
  sessionUpdate: "reminder_deduped"
  /** The newer reminder id that won the dedupe contest. */
  reminderId: string
  dedupeKey: string
  /** Reminder ids that were dropped because they shared the dedupe key. */
  droppedReminderIds: string[]
  _meta?: Record<string, unknown>
}

export interface SessionUpdate_ReminderExpired {
  sessionUpdate: "reminder_expired"
  reminderId: string
  /** Why the reminder ended its lifecycle. */
  phase: "ttl_expired" | "cleared" | "compacted_out"
  expiredAtTurn?: number
  _meta?: Record<string, unknown>
}
```

These three updates together let an editor or notebook UI render a
"reminders panel" alongside the transcript without polling.

### Capability negotiation

Agents that support reminder injection advertise it during
`initialize`:

```typescript
export interface AgentCapabilities {
  // ...existing fields...
  reminders?: {
    /** Agent accepts session/inject_reminder. */
    inject: boolean
    /** Agent emits reminder_emitted / reminder_deduped / reminder_expired updates. */
    emit: boolean
    /** Supported propagate values; absent means [\"session\"] only. */
    propagate?: ("all" | "session" | "none")[]
    /** Supported roleHint values; absent means [\"system\"] only. */
    roleHints?: ("system" | "developer" | "user_block" | "ephemeral_cache")[]
  }
}
```

Hosts that observe an agent without `reminders.inject` should fall back
to either suppressing the reminder, surfacing it to the user as a
sidebar notification, or — only as a last resort — emitting it as a
prefixed user message with a clear visual marker.

## Compatibility and migration

### From the current `_meta` envelope

Harn ships today with:

- `session/remind` JSON-RPC method (same shape as the proposed
  `session/inject_reminder` modulo the name).
- Per-reminder fields under `params._meta.harn.reminder` for any
  non-standard lifecycle hints.
- Outbound `_meta.harn.reminder` decorations on `session/update`
  notifications so observing clients can pull reminder data out of
  the existing tool-call/message updates.

Migration steps once the upstream schema lands:

1. Add `session/inject_reminder` as an alias for the existing
   `session/remind` handler; mark `session/remind` deprecated in the
   `_meta` extension contract.
2. Promote per-reminder lifecycle fields from
   `_meta.harn.reminder.*` to top-level standardized fields on
   `SessionInjectReminderRequest`.
3. Emit `SessionUpdate::ReminderEmitted` from the existing reminder
   render path; drop the `_meta.harn.reminder` decoration on
   tool-call/message updates once consumers migrate.
4. Regenerate `spec/protocol-artifacts/` via
   `make gen-protocol-artifacts`.

No-op for older clients: the `session/remind` alias and `_meta.harn`
fall-back stays available for at least one ACP minor version after the
standardized fields ship.

### For other ACP agents adopting this proposal

Agents that don't already render reminders can implement
`session/inject_reminder` as a thin wrapper that prepends a
provider-specific system block (or developer-role message) on the next
turn, with TTL=1, no dedupe, no propagation. That's strictly stronger
than the synthetic-user-message workaround and requires no transcript
schema work.

## Reference implementation status

| Surface | Status | Notes |
|---|---|---|
| `session/remind` JSON-RPC handler | Shipping (v0.8.x) | `crates/harn-serve/src/adapters/acp/mod.rs` |
| Typed `SystemReminder` lifecycle envelope | Shipping | `crates/harn-vm/src/llm/helpers/transcript.rs` |
| Dedupe lifecycle event log | Shipping | `transcript.reminder.deduped` on the EventLog |
| Expiry lifecycle event log | Shipping | `transcript.reminder.expired` on the EventLog |
| `SessionUpdate::ReminderEmitted` wire shape | Reference impl tracked in [#1828](https://github.com/burin-labs/harn/issues/1828) | Will emit under `_meta.harn.reminder` until upstream lands |
| Provider-capability rendering (developer vs system vs Anthropic user block) | Shipping | See [System reminders > Capability-aware rendering](../system-reminders.md#capability-aware-rendering) |

The canonical `SystemReminder` struct (verbatim from
[`crates/harn-vm/src/llm/helpers/transcript.rs`][reminder-rs]) maps
field-for-field to the TypeScript shape above:

```rust
pub struct SystemReminder {
    pub id: String,
    pub tags: Vec<String>,
    pub dedupe_key: Option<String>,
    pub ttl_turns: Option<i64>,
    pub preserve_on_compact: bool,
    pub propagate: ReminderPropagate,    // all | session | none
    pub role_hint: ReminderRoleHint,     // system | developer | user_block | ephemeral_cache
    pub source: ReminderSource,          // stdlib_provider | hook | bridge | in_pipeline | inherited
    pub body: String,
    pub fired_at_turn: i64,
    pub originating_agent_id: Option<String>,
}
```

## Open questions for upstream maintainers

1. **Naming.** `session/inject_reminder` mirrors `session/inject` for
   symmetry. ACP #1224 originally proposed `session/remind`. Either is
   defensible; we recommend `session/inject_reminder` so the verb
   pattern is consistent with the user-input sibling.
2. **`mode` semantics.** Should reminders honor the same
   `interrupt_immediate` / `finish_step` / `audit_only` delivery
   modes as `session/inject`, or always behave as `finish_step`
   (i.e., commit at the next turn boundary)? Our reference impl defers
   to whatever the host supplies and defaults to `finish_step`.
   (Note: `audit_only` was originally named `wait_for_completion`;
   the rename happened in harn#2212 because the model never sees
   reminders drained at loop exit, so "wait_for_completion" was
   misleading.)
3. **`roleHint` realism.** `user_block` and `ephemeral_cache` exist
   today because Anthropic models materially prefer a user-role
   content block with prompt-cache annotation. Should ACP standardize
   these hints (acknowledging provider differences) or stay
   provider-agnostic with just `system`/`developer` and leave the
   block-vs-system decision to the agent?
4. **Propagation across sub-agents.** ACP doesn't currently model
   sub-agent sessions as first-class — that's a host-side construct.
   We've found `propagate` essential in practice; should it ship as
   part of this proposal or wait for a sub-agent RFC?
5. **Sibling updates.** Are `reminder_deduped` and `reminder_expired`
   in scope for the first iteration, or should we ship only
   `reminder_emitted` and add the lifecycle siblings in a follow-up?
   Our experience says hosts need at least dedupe visibility to render
   a non-flickering reminder UI.
6. **Capability shape.** Is `agentCapabilities.reminders` the right
   key, or should reminder support fold into existing
   `agentCapabilities.sessionUpdate` / `agentCapabilities.input`
   substructures?

## References

- [ACP #1224 — `session/remind` discussion][acp-1224]
- [ACP #1220 — `session/inject` discussion][acp-1220]
- [Harn ACP/MCP extensions v1](../spec/harn-extensions/v1.md)
- [System reminders user guide](../system-reminders.md)
- [`SystemReminder` struct][reminder-rs]
