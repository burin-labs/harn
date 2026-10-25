# Ambient signals

A developer running a coding agent in a long session is the last to learn that
the default branch went red, that a runner pool went offline, or that a
production alert fired. The agent could act on that news, but nothing tells it
mid-conversation. Ambient signals close that gap. A configured condition becomes
an alert. Harn routes the alert to the best running agent session on the machine
and delivers it as a system reminder at the session's next safe point. The agent then
handles it or decides it is not its job.

This page explains the design and its security model. For setup, see
[Deliver signals to Claude Code and Codex](../signals-hooks.md). For commands,
files, and configuration, see the [signals reference](../signals-reference.md).

## The pieces

A **signal** is one condition with a lifecycle, such as "CI on `main` of
`acme/api` is failing". Its content is a `harn.triage_event.v1` envelope from
[`std/triage`](../connectors/triage-inbox.md), the same normalized card that
connectors already produce, so a signal has a mandatory `source_url`, a
summary, why it matters, and a proposed action. The signal adds what an inbox
card lacks: a lifecycle, a severity, routing hints, and an expiry.

The **inbox** is a durable store on the machine, under `~/.harn/signals/` by
default. Every producer writes into it and every delivery reads from it.

The **session registry** lives in the same store. A running agent session
announces itself with its id, harness kind, working directory, repository,
title, whether it is busy or idle, and when it was last seen.

The **router** decides which session should hear about each signal.

**Delivery adapters** hand a routed reminder to a session: a hook command for
Claude Code and Codex, and the `std/signals` builtins for an agent session
running in the Harn VM, which deliver through the session's system reminders.
Every adapter renders a reminder as a system or developer note. None delivers
it as a user message, because an alert must never carry the user's authority.
Claude Code's idle wake needs care here: Claude Code shows an `asyncRewake`
hook's output as a user-role turn labeled `Stop hook feedback`. The watcher
therefore prints one fixed sentence saying a signal is pending, never any
signal text, and the alert arrives through the `Stop` hook's
`additionalContext` when the woken turn ends. A live run of Claude Code
2.1.310 confirmed that a `Stop` hook's `additionalContext` continues the turn
and that the model acts on it.

**Monitors** are Harn scripts that poll a source on a schedule and fire or
resolve signals. Webhook payloads from alerting tools enter the same way,
normalized to the envelope.

## One reminder per state change, delivered at least once

A signal is `fired`, `resolved`, or `expired`. Firing a signal that is already
fired is not news: the inbox extends its expiry and delivers nothing. The text
stays frozen for the generation, so a redelivery reads exactly like the first
delivery. Each real transition increments the signal's generation, and each
generation is owed to one session. A rise in severity is also a new
generation, because an alert that got worse is news. A monitor that polls
every minute while CI stays red therefore produces one reminder when CI breaks
and one when it recovers, not sixty an hour.

Delivery is at least once. When a session checks in, the inbox **leases** it
the reminders routed to it; it does not mark them delivered. Each lease
carries a token, and only that token acks it: a hook acks the leases it
printed once its output is flushed, and an in-process delivery acks each lease
once the reminder is injected. Nothing else acks a lease, so a hook running
beside another one of the same session, as parallel tool hooks do, never acks
what its sibling leased. A lease nobody acks lapses after ten minutes by
default and is leased again with the next attempt number: to its holder while
that session is responsive, otherwise to a responsive session the router picks.
A session that has been silent for a lease window, because it crashed or is
deep in one long tool call, loses a reminder with no lease out to such a
session. A hook killed after leasing but before printing therefore loses
nothing; two deliveries may reach the model. Rendering is keyed by the
reminder's id, `key#generation`, so a redelivery reads the same, says it was
sent again, and replaces rather than repeats an in-process reminder for the
same signal. A design that marked reminders delivered when they were taken
would lose every reminder taken just before a crash, and one that acked on the
next check-in would let a sibling hook ack output that never reached the
model.

Expiry exists for conditions that may never be explicitly resolved, such as a
one-off production alert. A fired signal that nothing fires again within a day
(configurable) expires even when its producer set no expiry, so an alert
nobody resolves cannot stay pending forever. An expired signal produces a reminder only if its
fired reminder was leased to a session. A session that was told about something learns
that it lapsed; a session that never heard about it is not told about it now.

The signal's identity is the triage envelope's `dedupe_key`, which hashes
source provenance rather than delivery ids. A redelivered webhook therefore
maps to the same signal.

## Severity decides what may interrupt

Severity is `info`, `warning`, or `urgent`. Every severity reaches a working
session at its next safe point. Only `urgent` keeps a session that is about to
stop working, or wakes an idle one. Interruption is reserved for urgent signals
because it has a cost: an agent interrupted for every red pull request stops
making progress.

## One owner for muting, minimum severity, and quiet hours

The inbox applies the delivery policy when it routes, from `config.toml` in the
inbox directory. A muted signal, one below the minimum severity, or a
non-urgent signal during quiet hours is held: it stays in the inbox, unrouted,
and routes when the policy allows. Holding it in the router means every
harness, pipeline, and host that reads the inbox agrees on what reaches a
session, and a held signal is never leased, so it cannot be lost to a consumer
that filtered it after taking it. A host with its own settings screen writes
those settings into `config.toml` rather than filtering on its side.

## Routing

Routing is deterministic first:

1. Send the signal to a live session whose repository matches the signal's
   repository hint, or whose working directory contains the signal's
   working-directory hint.
2. Otherwise send it to a live session that registered the fallback role
   (`control` by default).
3. Otherwise hold it in the inbox. A held signal is never dropped; it is routed
   when a suitable session appears.

Ties go to the most recently seen session, then to the lexically smallest
session id, so two concurrent routing passes reach the same answer. Once a
generation is assigned to a session, the assignment sticks while the session
stays responsive, which keeps a reminder from bouncing between sessions. It
moves when the session ends, goes stale, or stays silent for a lease window
while another suitable session is responsive.

The rule step is a trait, not a hard-coded function, so a triage pipeline can
replace it. That pipeline is an LLM-gated Harn workflow that uses the trigger
system's existing gate and replay cache, which makes its decisions reproducible
without a fresh model call. It sits behind a flag because a model call per
signal is a cost and a latency the deterministic rules usually do not need.

## Why the inbox is one file under a lock

Many sessions on one machine run hooks concurrently, and every hook may write:
it records a heartbeat, leases reminders, and acks what it delivered. The inbox
is a single JSON document, `inbox.json`, that carries a schema version. Every
transaction takes an exclusive advisory lock on a sibling lock file, reads the
document, applies the change, writes a temporary file, and renames it into
place. Readers therefore never see a torn write, and two writers never lose
each other's changes. Routing runs inside the same transaction as delivery,
so two sessions cannot both hold a lease on one generation: the second reads
the first one's assignment.

The alternative, one file per signal, makes routing a multi-file transaction.
SQLite would work, but a small, inspectable document is enough for the volume
a single machine produces, and garbage collection keeps it small: resolved and
expired signals are pruned after a retention window, stale sessions are
removed, and the inbox holds at most `max_signals` signals (1,000 by default).
Past the cap, the oldest settled signal is pruned to make room; when every
signal is still fired, firing a new key is refused with an error the producer
sees, never dropped silently.

## Liveness

A session is live while its last heartbeat is newer than the stale threshold.
A busy session heartbeats on every hook. An idle session would look dead, so
the idle watcher that can wake it also heartbeats while it waits. When a
harness reports that a session ended, its registry entry is removed and every
reminder assigned to it and not yet acked, leased or not, returns to the
router. Each transaction holds the lock for milliseconds and gives up after two
seconds, well inside a hook's timeout, so a wedged holder fails one hook
instead of hanging it.

## Delivery at safe points

A reminder arrives where the harness already pauses: after a tool call, when
the person submits a prompt, when a session starts, and when the agent tries to
stop.

- After a tool call, on prompt submit, and at session start, up to three
  pending reminders are added to the model's context, and the output says how
  many more wait for the next safe point. Three worst-case reminders fit inside
  the smallest hook budget a supported harness enforces: Claude Code caps
  `additionalContext` at 10,000 characters, and Codex caps model-visible hook
  output near 2,500 tokens.
- When the agent tries to stop, only urgent reminders keep it going. Others
  wait for the next prompt.
- An idle Claude Code session can be woken only for urgent reminders, through
  an `asyncRewake` watcher that waits in the background and wakes the session
  with a fixed notice when one is routed to it; the reminder follows as
  `Stop` context when the woken turn ends. The watcher starts beside the
  `Stop` hook that leases what is already pending, so it gives a reminder
  pending at its first poll two more polls to be leased before it wakes for
  it; one routed after that hook ran is not missed. One watcher runs per
  session: a second one exits at once, so repeated stops never stack watchers.
  A watcher removes its lock file as it exits; session end, and the next write
  to the inbox, remove one a killed watcher left.

Codex differs at two points. Its `Stop` hook can keep a session going only by
turning its reason into a new user prompt, so the Codex adapter never delivers
there, and an urgent signal reaches a stopping Codex session at its next prompt
or tool call. Codex has no hook that wakes an idle session. These are gaps, not
emulated behavior.

## Security model

**A reminder is a notification, not an instruction.** The rendered text says
so plainly. It reports what happened, why it might matter, where it came from,
and a proposed action, and it leaves the decision to the agent and the person
directing it. Nothing in the signal path takes an action.

**Source content is untrusted.** A Sentry error message, a pull request title,
or a webhook body can contain text written to look like an instruction. The
renderer places every source-derived field, the signal id and producer
included, inside a `signal-data` block. It substitutes `&`, `<`, `>`, and `"`
with their entities and turns line breaks, other control characters, Unicode
line separators, bidirectional marks and overrides, zero-width characters,
and tag characters into spaces, so no content can
close the block, impersonate harness markup, or start a line of its own; the
agent still reads what the source said. Every field is capped in length. The
lines outside the block carry only values Harn chose: state, severity,
signature status, and whether this is a redelivery. The inbox also refuses a
signal key that contains a control character, and the command line prints
source text to a terminal through the same character filter as the renderer.
The inbox refuses an event whose summary exceeds 2,000 characters, that lists
more than 32 actors, related references, or action intents, or that exceeds
32 KiB without its raw payload, which is never stored. The model sees exactly one frame, written by Harn,
around data it should read but not obey.

**Provenance is mandatory.** Every signal names its producer, such as
`std/signals.monitor_github_ci`, and its signature status: `verified` for a
webhook whose signature the receiving trigger checked, `unverified` for one it
did not, and `local` for a condition observed on the machine. The reminder
shows both beside the envelope's `source_url`, so an agent can weigh the claim
and check it at its source. A report about an existing signal may not be
weaker than the signal's own: an unverified webhook cannot resolve, re-fire, or
extend a verified or local signal, and the inbox refuses it with an error.

**The proposed action is Harn's.** The monitor pack and the alert normalizers
write why a signal matters and what to do next themselves; they copy only the
summary and the link from a payload, never a suggested action.

**The store is private to the user.** The inbox directory is created with
owner-only permissions. Signals keep the normalized envelope; producers should
not put credentials in it, and the monitor pack never does.

**Local first, hosted later.** Monitors poll from the machine, so no public
ingress is required. Hosted webhook ingress belongs to a hosted control plane,
which writes the same envelope into the same inbox contract and adds team-wide
routing; it does not redefine any of the semantics on this page.

## Configuration through `HARN_EXT_SIGNALS_DIR`

The inbox location is overridable through `HARN_EXT_SIGNALS_DIR` rather than a
new `HARN_*` name. Harn rejects unknown `HARN_*` environment variables at
startup, so a new name exported in a shell profile would break every older,
pinned Harn binary on the machine. The `HARN_EXT_` namespace is accepted by
every version.
