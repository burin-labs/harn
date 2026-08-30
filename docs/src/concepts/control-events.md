# Stop, steer, and queue as control events

A control event is a person telling a running session to stop or to change
course. Harn treats it as an event the session must answer, not as a request
the session may finish its current plan first and then consider.

This page explains why the boundary sits where it does. For where the loop
drains an injection, see [Steering seams](./steering-seams.md). For the
per-tool-call variant, see `session/cancel_tool_call` on that same page.

## A stop is three guarantees, not one

Stopping is usually described as one thing and implemented as one thing, which
is why it usually half-works. It is three:

1. **The loop stops.** No further iteration, model call, or tool dispatch.
2. **What the session started dies.** A cancelled agent that leaves a
   backgrounded command running has relocated the work, not stopped it.
3. **The record says a person stopped it.** The prompt's terminal names the
   control.

The first is the one everybody implements and the only one a demo shows. The
other two fail quietly. A leaked child keeps burning a machine nobody is
watching. A terminal that does not name the stop makes a stopped run and a run
that merely produced nothing the same row in every downstream report — and the
difference between those two matters exactly when someone is trying to work
out why a fleet of runs went nowhere.

## Why an accepted cancel is the hard case

The intuition is that a cancel makes the turn *fail to finish*, so tagging the
outcome when a turn fails to settle should catch it.

It does not, because an accepted cancel **is answered**. The runtime unwinds
the agent loop and replies to the prompt with `stopReason: "cancelled"`. The
drain settles. The turn looks, to every structural check, exactly like a turn
that ended normally and happened to produce no assistant message.

So the observed control is authoritative over the settled/unsettled
distinction, not subordinate to it. If a stop was observed, the record says
so, regardless of how tidily the turn wrapped up.

## Absence must not read as success

The failure mode that survives review is a control that names something which
is not there.

A `session/cancel` naming a session the server has not registered used to be
consumed silently. Nothing was cancelled, and the frame was gone. From the
outside, "nothing was cancelled" and "everything was cancelled" produced the
same observable: no error, no event, no further output about it.

A miss is now loud. The server warns and emits a `ControlOutcome` with
`status: "rejected"`, `outcome: "unknown_session"`, and the session id it could
not find. Every control decision — accepted or rejected — produces one of these
records, carrying method, outcome, status, actor, target, and reason. Read the
structured record rather than grepping prose for a verdict.

This also shapes how the behavior is tested. A positive test that a cancel
stops the loop is passed just as well by an implementation that cancels
*everything*, so it needs a sibling negative control: a cancel naming an
unknown session must leave the live loop running. And a kill of a session's
background handles needs the id it keys on asserted equal to the session id the
control names, because a kill on a mismatched id is a no-op that reads as
success.

## Three things a mid-turn message can mean

Someone types while the agent is working. They mean one of three things:

| Intent | Mechanism | Owner |
|---|---|---|
| "Do this instead, at a sensible point" | `session/inject` mode `steer` | Harn |
| "Do this now, before the next tool" | `session/inject` mode `interrupt_immediate` | Harn |
| "Do this after you finish" | a new turn, started later | the host |

A runtime that assumes one of the three is wrong two-thirds of the time, so
this is a setting rather than a behavior.

The third is deliberately not Harn's. "After this turn" means a *new* turn,
which is a client-side decision about what to submit and when; Harn has no
say in it and should not pretend to. A host that queues must also discard the
queue when a stop lands — a stop that then runs whatever the person had lined
up behind it has deferred, not stopped.

Where a host exposes the choice as configuration, it should be a typed enum
with exactly one wire spelling per behavior, and an unrecognized value must be
reported rather than silently defaulted. A control policy that quietly reverts
to the default is one the operator believes is in force while it is not, which
is worse than having no setting at all.

## Signals are controls

A headless or supervised run has an operator too. SIGINT is that operator
saying stop now; SIGTERM is a supervisor asking the run to wind down. Both are
answered the same way an interactive stop is: one `session/cancel` on the
running session, because that is the path the runtime already terminates
cleanly on. Building a second stop path for signals would mean maintaining two
answers to the same question.

Two details carry weight. The signal races the **frame-receive point** rather
than the whole drain, so cancelling does not discard the frames the turn
already accumulated — the partial turn survives and still reports what it
observed. And the post-cancel wait is bounded, so a wedged engine cannot hold
the process open after the operator already said stop; a second signal skips
the wait entirely.

The two signals stay distinguishable in the recorded reason. A supervisor
winding a run down and a person hitting Ctrl-C are different facts about what
happened, and collapsing them costs you that later.
