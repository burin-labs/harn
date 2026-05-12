# Dashboard job envelopes

`std/dashboard/jobs` defines the portable status event Harn emits for Jobs
surfaces in local Burin and Harn Cloud. The contract is intentionally a
dashboard-facing envelope, not a scheduler API: Harn runtimes and connectors
produce ordered events, and hosts render queue/run/approval/DLQ/receipt state
from normalized fields.

```harn
import { dashboard_job_event, dashboard_jobs_view } from "std/dashboard/jobs"

let event = dashboard_job_event(
  "run.started",
  {
    job_id: "daily-digest",
    run_id: "run-001",
    source_timestamp: "2026-05-11T14:01:10Z",
    workflow_id: "workflow://local/start-my-day",
  },
)

let view = dashboard_jobs_view([event])
println(view.jobs[0].status)
```

## Event envelope

`dashboard_job_event(kind, input, options?)` returns
`harn.dashboard_job_event.v1`:

| Field | Purpose |
|---|---|
| `kind` | Event kind such as `run.queued`, `approval.requested`, `dlq.created`, or `receipt.available` |
| `source` | Runtime and environment, for example local Harn or Harn Cloud |
| `source_clock` | Source clock label plus timestamp for replayable ordering |
| `tenant_id`, `workspace_id` | Optional partition keys for cloud and multi-workspace hosts |
| `job_id` | Stable dashboard job key |
| `run_id`, `workflow_id`, `trigger_id`, `binding_id`, `event_id` | Runtime lineage where applicable |
| `status` | Host-renderable state: queued, running, waiting approval, succeeded, failed, DLQ, canceled, and related states |
| `queue`, `queue_depth`, `priority`, `attempt`, `max_attempts` | Queue and retry rendering hints |
| `approval` | First-class approval wait/decision state for approval events |
| `dlq` | First-class DLQ state for replay and dismissal flows |
| `receipt_refs`, `replay_refs`, `related_refs` | Links or ids for audit receipts, replay fixtures, and source records |
| `action_intents` | Host-owned actions such as opening the job, reviewing an approval, replaying DLQ, or dismissing DLQ |
| `raw_payload` | Original local or cloud payload kept separate from normalized render fields |

`source_clock.timestamp` is mandatory. Run events require `run_id`, approval
events require `approval.request_id`, DLQ events require `dlq.dlq_id`,
`receipt.available` requires at least one receipt ref, and
`replay_fixture.available` requires at least one replay ref.
Set `raw_payload_retained: false` when building an event to keep normalized
status fields while omitting the original runtime payload.

## Event kinds

The stdlib maps these event kinds to default statuses:

| Kind | Default status |
|---|---|
| `scheduled_job.created`, `scheduled_job.updated` | `scheduled` |
| `scheduled_job.canceled` | `canceled` |
| `run.queued` | `queued` |
| `run.started`, `run.progress`, `approval.approved` | `running` |
| `approval.requested` | `waiting_approval` |
| `run.succeeded` | `succeeded` |
| `run.failed`, `approval.denied`, `approval.expired` | `failed` |
| `dlq.created` | `dlq` |
| `dlq.replayed` | `queued` |
| `dlq.dismissed` | `canceled` |
| `receipt.available`, `replay_fixture.available` | `unknown`; these add refs without changing the current card status |

Callers can override `status` when a runtime has a more specific local state,
but hosts should be able to render a useful card from the default mapping.

## Static Jobs view

`dashboard_jobs_view(events, {emit?})` dedupes an ordered event stream and
reduces it into `harn.dashboard_job_card.v1` cards. The reducer is intentionally
order-based: deterministic fixtures can drive a static Burin Home Jobs view
without a live scheduler or database.

```harn
import { dashboard_job_event, dashboard_jobs_view } from "std/dashboard/jobs"

let events = [
  dashboard_job_event(
    "run.succeeded",
    {job_id: "daily-digest", run_id: "run-001", source_timestamp: "2026-05-11T14:03:00Z"},
  ),
]
let view = dashboard_jobs_view(events, {emit: true, topic: "dashboard.jobs.events"})
for job in view.jobs {
  println(job.job_id + ": " + job.status)
}
```

When `emit` is true, each unique event is appended to the EventLog as
`kind = "dashboard_job_event"` and the view returns
`harn.dashboard_job_event_emit_receipt.v1` receipts.

## Host boundary

Action intents are descriptive only. Non-navigation intents such as approval
decisions, DLQ replay, and DLQ dismissal must set `requires_approval: true`;
`dashboard_job_validate` rejects write-like intents that omit that gate. Hosts
own the approval UX, persistence, undo semantics, and concrete side effects.
