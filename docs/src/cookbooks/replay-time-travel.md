# Replay time-travel cookbook

`harn replay` rehydrates a recorded agent session from a SQLite EventLog
and projects it deterministically. With `--at <event-id>` you can rewind
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

## Ask "what if?" with `--counterfactual`

Rewinding shows you the state the agent saw. The next question is
*"what if it had edited differently?"* — answer it without mutating the
recorded session or the workspace. `--counterfactual <plan.harn>`
evaluates an alternate edit plan after the session has been rehydrated at
the `--at` cutoff and reports the **divergent file set**: the files the
plan's edits would touch.

```bash
harn replay --session-id sess_42 --events-db ./.harn/agent-events.db \
  --at 7 --counterfactual ./what-if.harn
```

The `.harn` plan `return`s an edit plan — the same ordered list of typed
ops [`edit_dry_run`](../stdlib/edit.md#edit_dry_run--preview-a-multi-op-plan)
accepts. (A bare trailing expression returns `nil` in Harn, so the plan
must use `return`.)

```harn
// what-if.harn — the edit the agent *could* have made at event 7.
return [
  {
    op: "safe_text_patch",
    path: "src/lib.rs",
    old_text: "fn greet()",
    new_text: "fn greeter()",
  },
  {
    op: "apply_node",
    path: "src/lib.rs",
    query: "(function_item body: (block) @target)",
    replacement: "{ format!(\"hi {name}!\") }",
    select: "first",
  },
]
```

(A single plan that prefers to call `edit_dry_run` itself works too —
`return edit_dry_run({plan: [...]})` — the divergence is read off the same
`per_file_unified_diff` / `summary` shape.)

The plan runner installs a copy-on-write filesystem overlay while it
evaluates the `.harn` file, then runs the returned ops through
`edit.dry_run`, which opens and immediately discards a throw-away
**staged-fs** overlay. Accidental `harness.fs.write_text(...)` / hostlib writes in the
plan program do not touch the working tree. The human output lists the
divergent files:

```text
Time-travelled to event 7: replaying the session as it stood at that point.
Replay: sess_42
...
Counterfactual: ./what-if.harn (ok)
  would touch 1 file(s) (+2 / -2 lines, 2 op(s) applied, 0 rejected):
    modified src/lib.rs (+2 / -2)
```

In `--json` mode the divergence rides on the replay report under
`data.counterfactual`:

```json
{
  "data": {
    "counterfactual": {
      "plan_path": "./what-if.harn",
      "plan_paths": ["./what-if.harn"],
      "step_count": 1,
      "result": "ok",
      "diverged": [
        { "path": "src/lib.rs", "status": "modified", "lines_added": 2, "lines_removed": 2 }
      ],
      "files_touched": 1,
      "lines_added": 2,
      "lines_removed": 2,
      "ops_applied": 2,
      "ops_rejected": 0
    }
  }
}
```

Each file's `status` is `created`, `modified`, or `deleted`, classified
from its line deltas.

**Counterfactual chains** can be one longer plan or repeated
`--counterfactual` flags. Repeated flags are evaluated in order and their
returned edit-op lists are concatenated into one `edit_dry_run`, so the
shared staged overlay collapses the cumulative effect into one diff per
file:

```bash
harn replay --session-id sess_42 --events-db ./.harn/agent-events.db \
  --at 7 \
  --counterfactual ./rename.harn \
  --counterfactual ./follow-up.harn
```

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

Because this projection is deterministic and the EventLog is append-only, the
audit is reproducible: the same `--session-id … --at N` always rehydrates
the same prefix.

## Re-execute a coding turn offline

A run record answers "what did the saved run say?" An offline coding replay
answers the stronger question: "does the recorded read, edit, and verification
trajectory still produce the same result from a clean workspace?"

Put the program, its recorded `--llm-mock` JSONL, and the initial files under
one seed directory. The program receives the recreated workspace path as
`argv[0]` and must return its `AgentResult`, including the producer-owned typed
terminal. Keep the files whose mutations matter under `effect_root`; run
artifacts can live elsewhere in the seed.

```json
{
  "_type": "offline_coding_replay",
  "schema_version": "harn.offline-coding-replay.v1",
  "workspace_seed": "coding-turn",
  "program": "turn.harn",
  "llm_mock": "turn.llm-mock.jsonl",
  "effect_root": "repo",
  "expected": {
    "tools": [
      {
        "id": "call-read",
        "name": "read_file",
        "status": "completed",
        "args_blake3": "1111111111111111111111111111111111111111111111111111111111111111",
        "result_blake3": "2222222222222222222222222222222222222222222222222222222222222222"
      },
      {
        "id": "call-edit",
        "name": "edit_file",
        "status": "completed",
        "args_blake3": "3333333333333333333333333333333333333333333333333333333333333333",
        "result_blake3": "4444444444444444444444444444444444444444444444444444444444444444"
      },
      {
        "id": "call-verify",
        "name": "run_command",
        "status": "completed",
        "args_blake3": "5555555555555555555555555555555555555555555555555555555555555555",
        "result_blake3": "6666666666666666666666666666666666666666666666666666666666666666",
        "exit_code": 0
      }
    ],
    "effects": [
      {
        "path": "src/lib.rs",
        "before_blake3": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "after_blake3": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      }
    ],
    "terminal": {
      "outcome": {"kind": "natural", "reason": "natural", "owner": "agent"},
      "exit_code": 0
    }
  }
}
```

Run it through the normal replay command:

```bash
harn replay --fixture fixtures/coding-turn.replay.json --json
```

Harn copies the seed to a new temporary workspace for every run, installs only
the recorded LLM fixture, runs through the ordinary sandbox with process
network disabled, and computes the final tree diff. The universal JSON envelope
keeps replay data under `data`; `data.producer` identifies the CLI version and,
for attested builds, source revision that performed the run. Each expected tool carries BLAKE3
digests of its canonical JSON arguments and semantic result. Harn normalizes
workspace paths and runtime-only identifiers before hashing results. Terminal
tool-event status, result digest, and any declared command exit code prove the
execution outcome, while typed mock-consumption checkpoints distinguish the
CLI tape from builtin fallback. The receipt names all five comparisons: tool
sequence, workspace effects, terminal verdict, provider isolation, and network
isolation. `pending_comparisons` and
`missing_comparisons` are always present. A missing returned terminal, empty
expected tool/effect evidence, an unmatched LLM prompt, an unexpected file
change, or any mismatch exits non-zero.

Use `--runs N` for repeated clean-room executions. Each run gets a new copy of
the seed, so a previous run cannot make a later comparison pass. The
`data.repeatability` receipt also compares every run's tools, effects, terminal,
and exit code with the first run.
