# Local workflow supervisor

`harn supervisor` is the stable local host surface for Burin GUI/TUI/CLI
automation management. It wraps the existing orchestrator runtime instead of
creating a second scheduler or queue implementation.

For a step-by-step walkthrough that introduces validation, preview,
local runs, connector status, and supervisor control commands together,
see the [workflow authoring quickstart](./workflow-authoring-quickstart.md).

The supervisor uses the same manifest and EventLog as `harn orchestrator`:

```bash
harn supervisor start --config harn.toml --state-dir .harn/orchestrator --mcp --json
harn supervisor list --config harn.toml --state-dir .harn/orchestrator --json
harn supervisor inspect cron-ok --config harn.toml --state-dir .harn/orchestrator --json
```

## Host contract

The JSON shape is intentionally compact and stable:

| Field | Purpose |
|---|---|
| `process` | Last supervised process pid, liveness, config, state dir, bind address, and log path. |
| `workflows` | One record per manifest trigger/workflow with status, provider, kind, handler, version, metrics, pause reason, and structured notification hints. |
| `events` | Stable host event names projected from trigger outbox, DLQ, and stranded inbox records. |
| `dlq` | Pending DLQ entries that can be replayed through `harn supervisor dlq replay`. |
| `catchup_needed` | Stranded inbox envelopes that need explicit recovery after crash/offline intervals. |
| `inspect` | The underlying orchestrator inspect payload for operators that need lower-level details. |

Stable event names are:

| Event name | Meaning |
|---|---|
| `workflow.scheduled` | A trigger/event was accepted or observed before dispatch. |
| `workflow.running` | Dispatch began or is running. |
| `workflow.blocked` | Dispatch is waiting on HITL, policy, or flow-control. |
| `workflow.failed` | Dispatch failed without landing in DLQ. |
| `workflow.completed` | Dispatch finished successfully or skipped deterministically. |
| `workflow.dlq` | Dispatch landed in the dead-letter queue. |
| `workflow.catchup_needed` | A persisted inbox envelope has no matching outbox dispatch and needs explicit recovery. |

Pause and resume are durable:

```bash
harn supervisor pause cron-ok --reason "waiting for user" --json
harn supervisor resume cron-ok --json
```

Paused workflow ids are stored in
`<state-dir>/workflow-supervisor-state.json`. The orchestrator applies that
state when it starts, reloads, or serves local CLI/API calls. If a supervised
process is already running, `pause` and `resume` send it a reload signal after
updating the state file. A paused workflow does not match incoming provider
events and targeted fire/replay attempts fail with a paused-binding error until
the workflow is resumed.

Each workflow's `notification_hint` includes `kind`, `workflow_id`,
`config_path`, and `state_dir` fields so hosts can construct native resume UI
without parsing command strings. The `resume_command` and `inspect_command`
fields remain convenience values for shell-oriented callers.

## Recovery and DLQ

Use DLQ commands for permanently failed events:

```bash
harn supervisor dlq list --json
harn supervisor dlq replay dlq_... --json
```

Use recovery commands for events that were persisted before the process stopped
but never reached the outbox:

```bash
harn supervisor recover --envelope-age 5m --dry-run --json
harn supervisor recover --envelope-age 5m --yes --json
```

Recovery uses the normal trigger replay path, so retry policy, flow control,
DLQ handling, receipts, and replay metadata remain consistent with live
dispatches.
