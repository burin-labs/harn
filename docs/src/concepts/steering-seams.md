# Steering seams

A *steering seam* is a point during a running agent loop where the runtime
checks for pending out-of-band influence: a queued user message, a system
reminder, an inbox feedback note, or a revocation. Where the seams are, and what
they honor, determines what kinds of "stop, do this instead" control a host can
exert.

This page describes the current state of the seams and where the gaps are. The
improvements tracked at
[issue #2211](https://github.com/burin-labs/harn/issues/2211) and
[issue #2213](https://github.com/burin-labs/harn/issues/2213) will reshape
this page substantially when they land.

## What you can inject

Three orthogonal channels feed into the loop:

| Channel | Producer | Drained from | Renders as |
|---|---|---|---|
| **Bridge injections** | `session/inject` and `session/inject_reminder` over ACP, or any host of the VM bridge | `__agent_loop_drain_bridge_step` (loop.harn:1558) | New user message or system reminder in the transcript |
| **Inbox feedback** | In-pipeline `agent_session_inject_feedback`, `agent_session_post_event`, command policy, MCP server hooks, stall diagnostics | Drained pre- and post-compact, currently only there | User-role messages |
| **Direct transcript inject** | `transcript.inject_reminder`, internal `agent_session_inject` | Appended directly when called | Whatever shape the caller built |

Bridge injections carry a **mode** — `interrupt_immediate`, `finish_step`,
`wait_for_completion` — that hints when delivery should happen.

## Where the seams are today

The agent loop body checks for pending steering at these points:

1. **Iteration top, after `turn_start`.** Both bridge and inbox drains.
2. **Stalled "done" path after `turn_end`.** Bridge drain only, to honor
   "continue if a steer arrived during done-judge."
3. **Post-iteration after `turn_end`.** Bridge drain only.
4. **Pre-compact and post-compact.** Inbox drain only, bracketing the compactor.
5. **Daemon idle wait, pre-sleep and post-sleep.** Bridge drain only,
   `interrupt_immediate` mode.
6. **Loop exit, before finalize.** Bridge drain only, `wait_for_completion`
   mode.

The atomic unit is **the iteration boundary**, not the tool-call boundary. There
is no drain between the model emitting a tool call and the tool executing.

## What this means for you

### Safe to inject between iterations

Anything you queue while the loop is between iterations will be visible to the
model on the next iteration's prompt. This includes both `interrupt_immediate`
and `finish_step` modes — today they're treated identically.

### Not yet safe to inject mid-tool

If the model has emitted a tool call and the dispatcher has started running it,
your steering injection will not interrupt the tool. The tool runs to
completion, and the reminder is drained at the next iteration top.

For destructive tools (anything that pushes to a remote, deletes files, sends a
message), this is a real gap. The proposed `cancel_in_flight_tool_call` builtin
([#2213](https://github.com/burin-labs/harn/issues/2213)) and the proposed
pre-tool-dispatch checkpoint
([#2211](https://github.com/burin-labs/harn/issues/2211)) both target this gap.

### `interrupt_immediate` is currently misnamed

The bridge mode `interrupt_immediate` does not preempt. It is drained at the
same seams as `finish_step` — between iterations. The "immediate" promise will
be honored when [#2211](https://github.com/burin-labs/harn/issues/2211) ships
the pre-tool-dispatch checkpoint.

If you're writing a host today and you need "actually stop the agent right now,"
your options are limited to:

- Call `session/cancel` to end the whole session.
- Inject with `interrupt_immediate` and accept that it lands at the next
  iteration boundary.
- Wait for the cancel-in-flight-tool-call primitive.

### `wait_for_completion` reminders are not rendered

A reminder queued with `mode: "wait_for_completion"` is drained at loop exit and
appended to the transcript, but no further LLM call runs to render it. The
reminder is visible in audit but invisible to the model. Tracked at
[#2212](https://github.com/burin-labs/harn/issues/2212).

If you want a reminder the model will actually see on its last response, use
`finish_step` and accept that delivery may land on either the final iteration or
the one after.

## The roadmap

The shape of an ideal steering API, as scoped in the open issues:

- A single named `__agent_loop_checkpoint(kind)` helper as the source of truth
  for what the loop drains where
  ([#2211](https://github.com/burin-labs/harn/issues/2211)).
- A pre-tool-dispatch checkpoint that honors `interrupt_immediate` by skipping
  the pending tool batch when a stop-shaped reminder arrives
  ([#2211](https://github.com/burin-labs/harn/issues/2211)).
- `register_checkpoint_hook(kinds, closure)` so plugin authors get one canonical
  extension point instead of juggling `turn_start` / `turn_end` / `post_compact`
  registrations ([#2211](https://github.com/burin-labs/harn/issues/2211)).
- `cancel_in_flight_tool_call(call_id, reason)` wired to per-tool cancellation
  tokens for real preemption of running tool calls
  ([#2213](https://github.com/burin-labs/harn/issues/2213)).
- `agent_session_pending_injections(session_id)` and
  `agent_session_revoke_reminder(...)` for hosts that want a "reminders panel"
  UI ([#2211](https://github.com/burin-labs/harn/issues/2211)).

When that work lands, this page will be rewritten as a reference of named
checkpoints rather than a description of inline drain sites.

## What hosts can do today

- Inspect already-delivered reminders via the transcript event log.
- Revoke a pending *user message* injection via `revoke_pending_user_message`
  (`bridge.rs:330`). Reminder revocation is not yet symmetric.
- Replace a pending *user message* via `replace_pending_user_message`
  (`bridge.rs:361`).
- Inject with `dedupe_key` to collapse duplicate reminders at the transcript
  level.
- Watch the `SessionUpdate::ReminderEmitted` event to know when a reminder was
  actually drained.

## Cross-references

- [System reminders](../system-reminders.md) — the user-facing API for queuing
  reminders.
- [ACP `session/inject_reminder`
  RFC](../protocol-contributions/acp-session-inject-reminder.md) — the
  protocol-side proposal.
- [Agent lifecycle](../agent-lifecycle.md) — suspend, resume, and self-park,
  which are different from steering but interact with it.
