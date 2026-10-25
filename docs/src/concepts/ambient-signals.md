# Ambient signals

A developer running a coding agent in a long session is the last to learn that
the default branch went red, that a runner pool went offline, or that a
production alert fired. The agent could act on that news, but nothing tells it
mid-conversation. Ambient signals close that gap. A configured condition becomes
an alert. Harn routes the alert to the best running agent session on the machine
and delivers it as a reminder at the session's next safe point. The agent then
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
Claude Code and Codex, and the in-process Rust API for hosts that speak ACP,
which deliver through `session/prompt` with `delivery: "queue"` or
`delivery: "steer"`.

**Monitors** are Harn scripts that poll a source on a schedule and fire or
resolve signals. Webhook payloads from alerting tools enter the same way,
normalized to the envelope.

## One reminder per state change

A signal is `fired`, `resolved`, or `expired`. Firing a signal that is already
fired is not news, so the inbox treats it as a refresh: it updates the stored
content and delivers nothing. Each real transition increments the signal's
generation, and each generation is delivered to exactly one session, exactly
once. A monitor that polls every minute while CI stays red therefore produces
one reminder when CI breaks and one when it recovers, not sixty an hour.

Expiry exists for conditions that may never be explicitly resolved, such as a
one-off production alert. An expired signal produces a reminder only if its
fired reminder was delivered. A session that was told about something learns
that it lapsed; a session that never heard about it is not told about it now.

The signal's identity is the triage envelope's `dedupe_key`, which hashes
source provenance rather than delivery ids. A redelivered webhook therefore
maps to the same signal.

## Severity chooses the delivery mode

Severity is `info`, `warning`, or `urgent`. The first two are delivered in
`queue` mode: the reminder waits for the session's next safe point and never
interrupts. `urgent` is delivered in `steer` mode: the session is asked to
consider the reminder now, and an idle session may be woken for it. Steering
is reserved for urgent signals because an interruption has a cost, and an
agent that is interrupted for every red pull request stops making progress.

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
generation is assigned to a session, the assignment sticks until the session
goes stale, which keeps a reminder from bouncing between sessions.

The rule step is a trait, not a hard-coded function, so a triage pipeline can
replace it. That pipeline is an LLM-gated Harn workflow that uses the trigger
system's existing gate and replay cache, which makes its decisions reproducible
without a fresh model call. It sits behind a flag because a model call per
signal is a cost and a latency the deterministic rules usually do not need.

## Why the inbox is one file under a lock

Many sessions on one machine run hooks concurrently, and every hook may write:
it records a heartbeat, takes delivery of reminders, and acks them. The inbox
is a single JSON document, `inbox.json`, that carries a schema version. Every
transaction takes an exclusive advisory lock on a sibling lock file, reads the
document, applies the change, writes a temporary file, and renames it into
place. Readers therefore never see a torn write, and two writers never lose
each other's changes. Routing runs inside the same transaction as delivery,
which is what makes "exactly one session" true: two sessions cannot both claim
a generation, because the second one reads the first one's assignment.

The alternative, one file per signal, makes routing a multi-file transaction.
SQLite would work, but a small, inspectable document is enough for the volume
a single machine produces, and garbage collection keeps it small: resolved and
expired signals are pruned after a retention window, and stale sessions are
removed.

## Liveness

A session is live while its last heartbeat is newer than the stale threshold.
A busy session heartbeats on every hook. An idle session would look dead, so
the idle watcher that can wake it also heartbeats while it waits. When a
harness reports that a session ended, its registry entry is removed and any
reminders assigned to it but not yet delivered return to the router.

## Delivery at safe points

A reminder arrives where the harness already pauses: after a tool call, when
the person submits a prompt, when a session starts, and when the agent tries to
stop.

- After a tool call, on prompt submit, and at session start, every pending
  reminder for the session is added to the model's context.
- When the agent tries to stop, only `steer` reminders keep it going. A `queue`
  reminder waits for the next prompt, which is what queueing means.
- An idle session can be woken only for `steer` reminders, through a watcher
  that waits in the background and exits with the reminder when one arrives.

Hooks that cannot add context at a given point are documented as gaps instead
of being emulated.

## Security model

**A reminder is a notification, not an instruction.** The rendered text says
so plainly. It reports what happened, why it might matter, where it came from,
and a proposed action, and it leaves the decision to the agent and the person
directing it. Nothing in the signal path takes an action.

**Source content is untrusted.** A Sentry error message, a pull request title,
or a webhook body can contain text written to look like an instruction. The
renderer places every source-derived field inside a fenced data block whose
boundary carries a random nonce chosen per render, and it neutralizes anything
in the content that could close the fence or impersonate harness markup. The
model sees exactly one authoritative frame, written by Harn, around data it
should read but not obey.

**Provenance is mandatory.** Every signal carries the envelope's `source_url`,
so an agent can check the claim at its source before acting on it.

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
