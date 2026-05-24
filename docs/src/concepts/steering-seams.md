# Steering seams

A *steering seam* is a point during a running agent loop where the runtime
checks for pending out-of-band influence: a queued user message, a system
reminder, an inbox feedback note, or a revocation. Every drain in the agent
loop now routes through a single named helper — `__agent_loop_checkpoint(kind)`
— so the set of seams is a closed catalog rather than a grep across the loop
body.

## What you can inject

Three orthogonal channels feed into the loop:

| Channel | Producer | Drained from | Renders as |
|---|---|---|---|
| **Bridge injections** | `session/inject` and `session/remind` over ACP; `agent_session_push_bridge_injection` for Harn-driven hosts | `__agent_loop_checkpoint` at every bridge seam (see below) | New user message or system reminder in the transcript |
| **Inbox feedback** | In-pipeline `agent_session_inject_feedback`, `agent_session_post_event`, command policy, MCP server hooks, stall diagnostics | `__agent_loop_checkpoint` at `pre_compact` / `post_compact` | User-role messages |
| **Direct transcript inject** | `transcript.inject_reminder`, internal `agent_session_inject` | Appended directly when called | Whatever shape the caller built |

Bridge injections carry a **mode** — `interrupt_immediate`, `finish_step`,
`audit_only` — that decides *which* seams drain it.

> **Note on `audit_only`.** This mode was previously called
> `wait_for_completion`. The rename (harn#2212) is truth-in-advertising:
> reminders queued with this mode land in the transcript at `loop_exit`
> but are **never rendered into a model prompt**. Hosts that need the
> model to react to a reminder before the agent terminates must use
> `finish_step`, which drains at every iteration boundary.

## The seam catalog

`__agent_loop_checkpoint(kind, ...)` fires at exactly these `kind` values, in
this order, per iteration:

| Kind | Where | Bridge modes drained | Inbox? |
|---|---|---|---|
| `turn_start` | Top of each iteration, after `turn_start` event | `interrupt_immediate`, `finish_step` | no |
| `pre_compact` | Just before `agent_autocompact_if_needed` | — | yes |
| `post_compact` | Just after `agent_autocompact_if_needed` | — | yes |
| `pre_tool_dispatch` | After `__invoke_llm` returns, before `__dispatch_tool_calls` | `interrupt_immediate` only | no |
| `turn_end` | Stalled-done-judge "done" path, before the loop falls through to terminal | `interrupt_immediate`, `finish_step` | no |
| `post_tool_dispatch` | After every successful turn dispatch | `interrupt_immediate`, `finish_step` | no |
| `daemon_idle_pre` | Daemon idle wait, before sleep | `interrupt_immediate` only | no |
| `daemon_idle_post` | Daemon idle wait, after sleep | `interrupt_immediate` only | no |
| `loop_exit` | After the loop body exits, before finalize | `audit_only` only (transcript audit, never rendered) | no |

Every checkpoint pass emits a `LoopCheckpoint` event carrying `iteration`,
`kind`, `delivered` (bridge injections drained at this seam),
`inbox_delivered` (feedback notes drained), and `dispatch_skipped`.

## `pre_tool_dispatch` is the new "stop" seam

The seam that didn't exist before #2211: between the LLM returning a tool call
and the dispatcher actually firing it.

When a host pushes an `interrupt_immediate`-mode injection (via ACP
`session/remind` or `agent_session_push_bridge_injection`), the
`pre_tool_dispatch` checkpoint drains it and returns `dispatch_skipped: true`.
The loop:

1. Skips `__dispatch_tool_calls` entirely — the tool batch does not run.
2. Records usage for the iteration so cost/token accounting stays honest.
3. Emits `turn_end` with `dispatch_skipped: true` and `skip_reason:
   "interrupt_immediate"` in the turn info.
4. Continues to the next iteration, where the injected reminder is already in
   the transcript and visible to the model on its next prompt build.

In other words, `interrupt_immediate` finally means *"stop before the next
tool fires"*, not *"land at the next iteration boundary anyway."*

The same `interrupt_immediate` injection arriving at `turn_start` or
`post_tool_dispatch` is still drained, but those seams sit between iterations
where no tool is pending — there's nothing to skip, the injection just lands in
the transcript and the next prompt sees it.

## `register_checkpoint_hook`

Plugin authors observe seams through one canonical builtin:

```harn,ignore
register_checkpoint_hook(["pre_tool_dispatch", "turn_end"], { event ->
  log("seam fired:", event.kind, "delivered:", event.delivered)
})
```

`kinds` accepts a single seam name, a list of seam names, or `nil` / `"*"` for
every seam. The handler receives the `LoopCheckpoint` payload directly.

Under the hood this registers a `loop_checkpoint` session hook with a
pattern derived from `kinds` — `register_session_hook("loop_checkpoint", ...)`
works too, with explicit pattern syntax (`kind=="pre_tool_dispatch"`,
`kind=~"^(turn_start|loop_exit)$"`).

## Migration from the old drain sites

Pre-#2211 code called `agent_session_drain_bridge_injections(session_id,
checkpoint)` directly at several sites. Those calls still work — the
checkpoint helper is implemented on top of them — but they bypass the
`LoopCheckpoint` event and the hook fan-out. Prefer
`__agent_loop_checkpoint` inside the loop body and `register_checkpoint_hook`
outside it.

The legacy `register_session_hook("turn_start", ...)` /
`register_session_hook("post_compact", ...)` triple-registration pattern
still works for backward compatibility, but `register_checkpoint_hook` covers
every seam in one call and exposes the `dispatch_skipped` signal that
turn-level hooks never see.

## Mid-tool preemption

Steering at iteration boundaries handles "stop before the next tool fires."
For the case where a tool is *already in flight* and the host wants to abort
*that* call — e.g. one click cancels a runaway `git push --force` without
losing the rest of the session — Harn ships
`cancel_in_flight_tool_call(session_id, call_id, opts?)` and the matching
ACP method `session/cancel_tool_call`. Both share a per-call cancellation
registry keyed by `(session_id, call_id)`:

```harn
cancel_in_flight_tool_call(
  "sess_abc",
  "call_42",
  {reason: "user clicked stop", inject_reminder: true, timeout_ms: 5000},
)
// → {status: "cancelled" | "already_cancelled" | "not_found" | "timeout",
//    call_id: "call_42", tool: "git_push", reason: "user clicked stop"}
```

The cancelled call returns to the loop shaped as
`status: "cancelled"` — distinct from `status: "error"` — so the model can
distinguish "the host stopped me" from "the tool failed." Tools written
against tokio's drop semantics unwind immediately; tools that hold
non-droppable resources (a `spawn_blocking` thread, an external process
without `kill_on_drop`) can additionally observe the cancellation handle
via the registry and shut down cooperatively.

## What's still out of scope

These were called out as separate issues in #2211 and are not yet shipped:

- **Bridge-level dedupe-key collapsing.** Multiple `interrupt_immediate`
  injections with the same `dedupe_key` are drained as separate transcript
  events instead of collapsing.

> `audit_only` reminders that drain at `loop_exit` and never render to the
> model are now expected behavior, not a bug — see
> [harn#2212](https://github.com/burin-labs/harn/issues/2212). Use
> `finish_step` if you want the model to see the reminder before the loop
> terminates.

## Cross-references

- [System reminders](../system-reminders.md) — the user-facing API for queuing
  reminders.
- [ACP `session/inject_reminder`
  RFC](../protocol-contributions/acp-session-inject-reminder.md) — the
  protocol-side proposal that depends on the `interrupt_immediate` semantics
  this page describes.
- [Agent lifecycle](../agent-lifecycle.md) — suspend, resume, and self-park
  interact with steering but are not themselves steering seams.
