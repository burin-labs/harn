# Replay time-travel cookbook

`harn replay` rehydrates a recorded agent session from a SQLite EventLog
and replays it deterministically. With `--at <event-id>` you can rewind
to **any past event** and replay the session as it stood at that point —
the foundation for auditing "what had the agent seen by the time it made
this decision?".

## Replay a whole session

Every agent session writes its events to a durable EventLog. Point
`harn replay` at that database and a session id:

```bash
harn replay --session-id sess_42 --events-db ./.harn/agent-events.db
```

The command reconstructs the run record from the session's events,
replays it, and reports the stages, transitions, and the replay-fixture
verdict. Add `--json` for the structured `JsonEnvelope` shape (see
[the CLI JSON contract](../cli-json-contract.md)).

## Rewind to a past event with `--at`

Agent-session events carry a monotonically increasing `event_id`. Pass
`--at <event-id>` to rehydrate only the prefix up to **and including**
that event — the session is replayed exactly as it stood at that moment,
with everything after the cutoff dropped:

```bash
# Replay sess_42 as it was right after event 7.
harn replay --session-id sess_42 --events-db ./.harn/agent-events.db --at 7
```

The cutoff is inclusive and need not name an event that exists — `--at 5`
over a session whose events are `[2, 4, 6]` keeps events `2` and `4`. A
cutoff that precedes the first recorded event is rejected with a clear
error rather than producing a silent empty replay.

In `--json` mode the source summary records the cutoff:

```json
{
  "source": {
    "kind": "event_log_session",
    "session_id": "sess_42",
    "events_db": "./.harn/agent-events.db",
    "at_event_id": 7
  }
}
```

The replay report's `transcript_event_count` reflects the truncated
prefix, so you can diff the determinism of "the session up to event N"
against the full run.

## Audit a past run, ask what-if, ship the fix

The typical loop:

1. **Find the decision.** Replay the whole session (`--json`) and scan
   the stages/transitions for the step you want to interrogate; note its
   `event_id`.
2. **Rewind.** Replay again with `--at <that-event-id>` to see exactly
   the context the agent had at that point — no later events leak in.
3. **Vary and verify.** Re-run the slice while changing the workspace or
   inputs the agent saw, and compare the new replay against the recorded
   one to confirm your fix changes the outcome you expected and nothing
   else.

Because replay is deterministic and the EventLog is append-only, the
audit is reproducible: the same `--session-id … --at N` always rehydrates
the same prefix.
