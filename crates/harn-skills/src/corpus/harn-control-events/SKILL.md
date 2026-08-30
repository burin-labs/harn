---
name: harn-control-events
short: Stop, steer, and queue as events a session must answer, not requests it may ignore.
description: Make an agent session answer stop and steer promptly, kill what it started, and leave a record that names the control.
when_to_use: Use when adding or auditing cancel, stop, steer, interrupt, or queued-message handling, or when a stop appears to work but leaves work running or a record that does not say it happened.
---

# Harn control events

A control event is a person telling a running session to stop or to change
course. It is an event the session must answer, not a request it may finish
its current plan first and then consider.

Pair this skill with [[harn-orchestration]] for where the loop drains
injections, [[harn-agent]] for loop and terminal vocabulary, and
[[harn-product-quality]] for what a host must show while a control is in
flight.

## The three things a stop has to do

A stop that does one or two of these is not a stop. Check all three
separately, because the second and third fail silently while the first
looks correct.

1. **The loop stops.** No further iteration, model call, or tool dispatch
   after the control lands.
2. **What the session started dies.** A cancelled agent that leaves a
   backgrounded command running has moved the work, not stopped it. Kill the
   handles owned by *that* session and no others.
3. **The record names the control.** The run's terminal says a person
   stopped it. Without this, a stopped run and a run that merely produced
   nothing are the same row in every downstream report.

Point 3 is the one that rots. An accepted cancel **is answered** — the prompt
returns `stopReason: "cancelled"` — so the turn settles normally and any
tagging that fires only when a turn fails to settle never fires at all. Seal
the terminal from the observed control, not from whether the turn looked
finished.

## Absence must not read as success

The dangerous shape is a control that names something that is not there.

- A cancel naming an unregistered session must be **loud**: warn, and emit a
  `ControlOutcome` with `status: "rejected"` naming the session. Consuming
  the frame silently makes "nothing was cancelled" indistinguishable from
  "everything was cancelled".
- Every control decision, accepted or rejected, gets a `ControlOutcome`
  carrying method, outcome, status, actor, target, and reason. Read the
  structured record; do not grep prose for the verdict.
- A record that a control *arrived* is not a record that the loop *stopped*.
  Assert the loop counters after the control, not the frame before it.

## Delivery modes for a mid-turn message

Typing while a session works means one of three things, and a runtime that
assumes one of them is wrong two-thirds of the time. Make it a setting.

| Mode | Wire value | Meaning |
|---|---|---|
| Steer | `steer` | Redirect the running turn at its next iteration or tool boundary. |
| Interrupt | `interrupt_immediate` | Deliver as soon as the loop will take it, ahead of the next tool. |
| Queue | *(host-side)* | Hold it and send it as a new turn once this one reaches its terminal. |

Steer and interrupt are `session/inject` modes and are the runtime's to
implement. **Queue is not**: "after this turn" means a new turn, which is the
client's decision, so the queue lives in the host. A host that queues must
also discard the queue on stop — a stop that then runs what the person had
lined up behind it has deferred, not stopped.

Give the setting a typed enum with one wire spelling per behavior. No aliases,
and never fall back to a default in silence: a control policy that quietly
reverts is one the operator believes is in force while it is not. Report the
rejected value and the mode actually running.

## Signals are controls too

A headless or supervised run answers SIGINT and SIGTERM by sending one
`session/cancel` on the running session — the same path an interactive stop
uses, because that is the path the runtime already terminates cleanly on.

- Race the signal at the **frame-receive point**, not around the whole drain,
  so a cancel does not discard the frames the turn already accumulated. The
  partial turn stays intact and still reports what it observed.
- Bound the post-cancel wait so a wedged engine cannot hold the process open
  after the operator said stop; a second signal skips the wait.
- Distinguish the two signals in the recorded reason. A supervisor winding a
  run down and an operator hitting Ctrl-C are different facts.

## Verify

Deterministic tests for the invariants; one re-runnable end-to-end check for
the claim.

- **Positive**: a cancel mid-loop leaves zero further iteration, model-call,
  and tool-dispatch events.
- **Negative control, required**: a cancel naming an unknown session must
  leave the live loop running. Without it, a fix that cancels everything
  passes the positive test.
- **Blast radius**: spawn two real background children on two sessions,
  cancel one, poll for that pid to die, and assert the other is still alive.
  Poll rather than reading once — the child is reaped a moment after the
  agent goes, and a single immediate reading reports a working stop as a leak.
- **Identity**: assert the id the background handles are keyed by equals the
  session id the control names. A kill on a mismatched id is a silent no-op
  that reads as success.
- **End to end**: keep a script that stops a real run and checks all three
  guarantees. Run it model-free with a mock provider so it stays free and
  anyone can re-run it.
