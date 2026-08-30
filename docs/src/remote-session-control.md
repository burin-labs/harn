# Remote session control

A control client is a program that drives an agent session it does not host. It
starts the session, watches it, stops it, redirects it, and picks it back up
later, all over the wire.

This page is the reference for that vocabulary. It has one job: for each control
word, name the exact ACP method that carries it, the receipt it produces, and
the guarantee you may rely on. Where the [A2A](./mcp-and-acp.md) protocol
already has a word for the same idea, this page names it too, so a peer harness
can interoperate without guessing. Where neither protocol has a word, this page
says so plainly instead of inventing one.

For *why* stop and steer are events rather than suggestions, read
[Stop, steer, and queue as control events](./concepts/control-events.md). For
the loop seams that make the timing work, read
[Steering seams](./concepts/steering-seams.md). For the full JSON-RPC method
list, read [MCP, ACP, and A2A integration](./mcp-and-acp.md).

## The control words

| Control word | ACP method | A2A term | Effect |
|---|---|---|---|
| Start | `session/new` | `message/send`, task state `submitted` | Create a session and run the first turn |
| Status | `session/list`, `harn.session_view.query` | `tasks/get`, task state `working` | Report whether a turn is running and what it is doing |
| Stop | `session/cancel` | `tasks/cancel`, task state `cancelled` | End the running turn now |
| Steer | `session/inject` mode `steer` | none | Add an instruction the turn picks up after the current tool batch |
| Queue | `session/inject` mode `queue` | none | Record a message that lands only after the loop ends |
| Interrupt | `session/inject` mode `interrupt_immediate` | none | Skip the pending tool batch and let the model read the message first |
| Resume | `session/load`, `session/resume` | `tasks/resubscribe` | Reattach to a session that is already on disk |

Two of these words have no A2A equivalent at all. A2A can send a message to a
task and it can cancel a task, but it has no way to say *when* during a running
turn that message should land. Steer, queue, and interrupt are the three answers
to that question, and they are Harn extensions to ACP. A peer harness that
speaks only A2A can start, watch, stop, and resume a session; it cannot steer
one.

## Start

`session/new` creates the session and returns its id.

```json
{"jsonrpc":"2.0","id":1,"method":"session/new",
 "params":{"cwd":"/path/to/project"}}
```

The result carries `sessionId`, a `session` object, the available `modes`, and
the `configOptions` for the current mode. The `session` object is the same shape
`session/list` returns, described under [Status](#status).

Sending work is a second call, `session/prompt`, with the session id and the
prompt content. The prompt call does not return until the turn reaches a
terminal, so a control client that wants to stop or steer must send those frames
on the same connection while the prompt response is still outstanding.

`cwd` is required for a session that will touch files. An optional
`environmentPolicy` narrows what the session may read from the host
environment; leaving it out means the session inherits.

The A2A path is `message/send`, which creates a task in state `submitted` and
moves it to `working` once the agent picks it up.

## Status

There is **no `session/status` method.** Status is answered by two existing
calls, and which one you want depends on the question.

`session/list` is the liveness question. Each entry carries:

| Field | Meaning |
|---|---|
| `sessionId` | The canonical id |
| `liveState` | `live` if the server holds it in memory, `persisted` if it is only on disk |
| `activePrompt` | `true` while a turn is running |
| `currentModeId` | The mode the session is in |
| `cwd` | The session's working directory |
| `lastEventId` | The cursor into the session's event log |
| `attachableRoles` | Which roles a client may claim |

`activePrompt` is the running-or-idle bit. `liveState` has exactly two values on
this surface; there is no `awaiting-input` value here.

`harn.session_view.query` is the richer question: what has this session actually
done. It returns the `harn.session_view.v1` projection, whose `run.status`
carries the aggregated session status and whose `pending` block names what the
session is blocked on — `pending.approvals` for a permission prompt,
`pending.auth` for a provider or connector login. That `pending` block is the
closest thing Harn has to "waiting for a human".

The A2A surface *does* have a single status call, `tasks/get`, and it does carry
an explicit `input-required` state. Harn's A2A adapter maps a session that is
waiting on a human to `input-required` and back to `working` when the answer
arrives. If your control client speaks A2A, that state is available to you; over
ACP you read `pending.approvals` instead.

## Stop

`session/cancel` ends the running turn. It takes only `sessionId` and works as
either a request or a notification. Sending it as a notification is the usual
choice, because the server reads inbound notifications ahead of routing request
responses, so the cancel does not queue behind the prompt it is trying to end.

Three guarantees, in order of how often they get assumed and not checked:

**A tool call that has not been sent is never sent.** The bridge checks the
cancel flag before it writes any host call, so every tool in a batch that was
pending when the cancel landed is skipped. This is the falsifier for a stop that
is only cosmetic: cancel during a batch, then assert the batch's later tools left
no trace.

**A tool call already in flight is abandoned, not undone.** The bridge stops
waiting for its result. The host may still finish running it. Harn does not roll
back its effect. A control client that needs the effect reversed needs its own
undo; the protocol does not offer one.

**Processes the session started are killed.** A backgrounded command outlives
the tool call that launched it by design, so unwinding the agent loop would
never reach it. Cancel reaches it explicitly.

The turn seals with ACP `stopReason: "cancelled"`. Do not confuse that with the
typed terminal kind, which is `user_cancelled` and rides in
`_meta.harn.terminal`. The wire word and the terminal word are different words
for the same event, and both appear in the same response.

Cancel is idempotent. The first one answers `status: "cancelled"`; every one
after answers `status: "already_cancelled"`. A cancel naming a session the
server does not hold is rejected with `-32004` and logged, so a stop aimed at a
dead session cannot be mistaken for one that worked.

Two narrower stops exist. `session/cancel_tool_call` stops one named tool call
without tearing down the session. `session/close` cancels, flushes the session's
event sinks, and drops it entirely.

## Steer

Steering adds an instruction to a turn that is already running, without changing
what the session was asked to do.

```json
{"jsonrpc":"2.0","id":7,"method":"session/inject",
 "params":{"sessionId":"…","mode":"steer",
           "content":[{"type":"text","text":"prefer the smaller fix"}]}}
```

The result is `{"messageId": "…", "status": "accepted"}`. Accepted means queued
for delivery, not delivered; the `messageId` is what you would pass to
`session/revoke_inject` or `session/replace_inject` to pull it back before it
lands.

A steered message is drained at three seams: the start of an iteration, just
after a tool batch finishes, and the end of an iteration. It is deliberately
*not* drained just before a tool batch dispatches, because "after the current
step" is the whole meaning of steer, and delivering it there would make it an
interrupt.

So the delivery guarantee is: a steer lands before the next prompt the model
sees, and never in the middle of a tool batch.

**Steer does not change the objective.** The session's goal is what it was asked
for at `session/prompt`. A steer adjusts how the agent pursues that goal. A
control client that wants a different objective starts a new turn.

### Authority

Every message Harn puts in front of a model carries an authority level, and the
levels are ordered:

| Authority | Rank | Used for |
|---|---|---|
| `contract` | 3 | What the session must do |
| `corrective` | 2 | A correction to how it is doing it |
| `advisory` | 1 | A suggestion |

When two directives collide, the higher authority wins outright; only at equal
authority does the more recent one win. The surviving directives are rendered to
the model in contract, then corrective, then advisory order, and the level is
literally an attribute the model reads:
`<directive authority="contract">`.

This ladder is why steer authority matters. A completion judge is an authority
gate, not an advisory reviewer: it re-derives what the session owes from the
original request and can emit a corrective directive restating it. **An operator
steer that carries no authority structurally loses to that judge** — the judge's
corrective directive outranks a plain user message, and the agent reverts to the
original wording one turn later.

The contract is therefore: a delivered steer registers a directive at
`contract` authority, so no completion judge can undo it. Registering steer at
contract authority is tracked in
[harn#7580](https://github.com/burin-labs/harn/issues/7580); until it lands, a
control client must treat a steer as advisory in practice and verify the final
answer against the steer rather than assuming it held.

## Queue

Mode `queue` records a message that is drained at exactly one seam: the end of
the loop.

A queued message lands in the transcript and is never rendered into a model
prompt, because no further model call runs after the loop exits. Queue is for
the audit trail, not for changing behavior. Its internal name is `audit_only`,
and you will see that word in the bridge and in the `session/remind` capability
list; over ACP the mode is spelled `queue`.

If you want the model to read a message before the agent terminates, use `steer`,
not `queue`.

A control client that means "after this turn, start a new one" is describing a
*second turn*, which is a decision the client owns. Hold the message and send it
as its own `session/prompt` once the running turn reaches its terminal.

## Interrupt

Mode `interrupt_immediate` is the only mode that can stop a tool batch from
running. It is drained at every seam steer is drained at, plus the seam just
before a tool batch dispatches, plus the daemon idle seams.

When one arrives at the pre-dispatch seam, the whole pending batch is skipped.
Each skipped call is recorded with a synthetic result marked `interrupted` and
the reason "a user interrupt arrived before this tool call was dispatched", the
loop checkpoint carries `dispatch_skipped: true` and
`skip_reason: "interrupt_immediate"`, and the loop continues so the model reads
the message where the tool results would have been.

Interrupt is not stop. The session keeps running; only the batch is dropped. Use
`session/cancel` to end the turn.

The mode also accepts the shorter spelling `interrupt` on the wire.

## Resume

Two methods pick a session back up, and the difference is whether you want the
history replayed at you.

`session/load` reattaches and replays. It first flushes the session's event
sinks, then re-emits every persisted event as a `session/update` notification
marked `_harn.replayed: true`, and returns a `replayed` list of
`{eventId, type}` pairs. Use it when your client needs to rebuild its own view of
what happened. Because the flush happens before the replay, a turn that just
completed is durable before the replay reads it.

`session/resume` reattaches without replaying. Same result shape, no
notifications, no `replayed` list. Use it when you already hold the history.

Both take only `sessionId`. A session that is still live in memory stays
promptable after either call; loading a live session does not turn it read-only.

### What "resumed from" can actually name

There is **no transcript hash** in Harn. A control client cannot ask "resume
this session and prove it is byte-identical to what I last saw", because no such
identifier exists. Naming one would be inventing a guarantee the system does not
make.

Three real identifiers do exist, and a receipt should carry whichever one
matches the claim:

| Identifier | Where it lives | What it pins |
|---|---|---|
| `lastEventId` | The session item from `session/list` | The cursor position in the event log |
| `projection_hash`, `prefix_hash` | The `harn.session_view.v1` projection | The content of the projected view, and of its prefix |
| `before_message_count`, `after_message_count` | A turn checkpoint | Transcript length either side of one turn |

`prefix_hash` is the closest thing to the guarantee people usually want: two
clients that agree on a prefix hash agree on the history up to that point.

### Turn checkpoints

A turn checkpoint is a snapshot of the transcript and filesystem taken when a
turn completes. Its id has the shape `turn_<uuid>`, and it is created only when
the transcript actually changed or a filesystem snapshot exists.

**A checkpoint is not addressable by id.** `session/rollback` and `session/redo`
take only `sessionId`; the `checkpointId` in the response tells you which
checkpoint was used, and there is no way to name a different one. Rollback pops
the last completed turn; redo pushes it back. Both refuse while a prompt is
running, answering `status: "prompt_active"`.

So "resume from a checkpoint" is two different operations depending on what you
mean. Reattaching to a session is `session/load` or `session/resume`. Undoing the
last turn is `session/rollback`. There is no operation that jumps a session to an
arbitrary earlier checkpoint.

## Receipts

Every control the server arbitrates emits one typed event, `control_outcome`,
which reaches clients over the `_harn/agentEvent` notification. It is emitted for
accepted, idempotent, *and* rejected controls, so a control that lost cannot be
silently indistinguishable from one that was never sent.

| Field | Meaning |
|---|---|
| `session_id` | The session the control named |
| `control_id` | The control's own id |
| `method` | The ACP method, for example `session/cancel` |
| `outcome` | What happened, for example `cancelled`, `already_cancelled`, `accepted`, `unknown_session` |
| `status` | `accepted` or `rejected` |
| `actor` | Who sent it |
| `target` | What it named, for example `{"sessionId": …}` or `{"messageId": …}` |
| `reason` | Why it was rejected, when it was |
| `metadata` | Extra typed context |

Read `status` and `outcome`, not the prose. A rejection reason is a sentence
written for a human; the two structured fields are the ones that mean something.

An accepted stop is `method: "session/cancel"`, `status: "accepted"`,
`outcome: "cancelled"`. The second stop on the same session is also
`status: "accepted"` but `outcome: "already_cancelled"` — accepted describes the
arbitration, not the effect. A stop naming an unregistered session is
`status: "rejected"`, `outcome: "unknown_session"`.

An accepted steer is `method: "session/inject"`, `outcome: "accepted"`, with the
assigned `messageId` in `target`. A steer with a mode the server does not know is
rejected with `reason: "invalid_mode"`; one with empty content is rejected with
`reason: "invalid_content"`.

The turn's own ending is separate. It is sealed on the prompt result as ACP
`stopReason` plus the typed terminal in `_meta.harn.terminal`, which names both a
kind and an owner. `user_cancelled` is owned by `user`; a budget stop is owned by
`policy`. A control client that wants to know *who* ended a turn reads the
owner, not the kind.

## What to verify, not assume

Four claims on this page are the ones worth proving against a live server rather
than trusting, because each one fails in a way that still looks like success.

**A stop stopped something.** Cancel mid-batch, then assert that the tools later
in that batch left no trace. A run that was going to finish anyway produces the
same transcript as a stop that did nothing.

**A steer survived.** Steer to a different final answer, then assert the final
assistant message matches the steer and not the original request. A steer that
was delivered and then reversed by a completion judge produces a clean
`control_outcome` receipt and the wrong answer.

**A resume resumed.** Compare the `prefix_hash` before and after, or count the
replayed events. A `session/load` against an unknown id errors, but a load
against a session with an empty log succeeds and replays nothing, which reads
identically to a session that replayed correctly and had nothing to say.

**A status call measured something.** `session/list` filtered to `liveState:
live` returns an empty list both when nothing is running and when the server
lost the session. Read `activePrompt` on a named session rather than inferring
liveness from an empty list.
