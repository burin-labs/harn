# Dashboard job/status events

`std/jobs` defines the portable `harn.job_event.v1` envelope that Harn local
orchestrators and managed Harn Cloud runners emit for host Jobs dashboards.
Hosts can render queue state, current status, approval waits, DLQ items,
receipts, and replay fixtures from the envelope without becoming a scheduler.

```harn
import { job_emit, job_normalize } from "std/jobs"

let event = job_normalize({
  event_kind: "run.queued",
  source_timestamp: "2026-05-11T09:01:00Z",
  job_id: "job_daily_digest",
  run_id: "run_daily_digest_001",
  workflow_id: "wf_daily_digest",
  queue: {name: "digest", depth: 3, position: 2},
})

job_emit(event)
```

`job_emit(input, {topic?})` validates the envelope, appends it as
`kind = "job_event"` to `jobs.status.events` by default, and returns
`harn.job_event_emit_receipt.v1` with the EventLog id.

## Envelope

`job_normalize(input, options?)` returns `harn.job_event.v1`:

| Field | Purpose |
| --- | --- |
| `id` | Stable event id, caller supplied or derived from source, job, run, event kind, and source timestamp. |
| `event_kind` | Normalized transition such as `run.queued`, `approval.requested`, or `dlq.created`. |
| `status` | Host-facing current state such as `scheduled`, `queued`, `running`, `waiting_approval`, `succeeded`, `failed`, `dlq`, `receipt_available`, or `replay_available`. |
| `source` | Orchestrator identity: `orchestrator`, `location` (`local` or `cloud`), optional version and clock metadata. |
| `source_timestamp` | Timestamp from the producing orchestrator clock. Hosts use it for ordering instead of inventing local receipt time. |
| `tenant_id`, `workspace_id` | Cloud/workspace routing identifiers when applicable. |
| `job_id` | Stable job identifier shared across schedule, run, approval, DLQ, receipt, and replay events. |
| `run` | Run identifiers such as `run_id`, `root_run_id`, `parent_run_id`, `workflow_id`, `trace_id`, and `trigger_event_id`. |
| `schedule` | Schedule metadata for scheduled jobs. Use cron/timezone/business-calendar fields from Harn trigger and `std/calendar` semantics. |
| `queue` | Queue depth and position fields a dashboard needs without reading worker internals. |
| `progress` | Current/total/percent and stage labels for in-flight work. |
| `approval` | First-class approval wait/decision state with request id, reviewers, quorum, deadline, reason, and link. |
| `dlq` | First-class dead-letter state with entry id, source event id, error class, retry count, and replay id. |
| `receipt` | Receipt id plus URL or path and receipt status. |
| `replay_fixture` | Replay/test fixture id plus URL or path and replay mode. |
| `result` | Last terminal result summary and error detail when available. |
| `links` | Source, receipt, and replay navigation links. |
| `metrics` | Optional cost/latency/count metrics. |
| `raw_payload` | Original producer payload retained for audit. |

## Event kinds

The v1 event kind set is intentionally small and dashboard-facing:

| Family | Kinds |
| --- | --- |
| Scheduled jobs | `scheduled_job.created`, `scheduled_job.updated`, `scheduled_job.canceled` |
| Runs | `run.queued`, `run.started`, `run.progress`, `run.succeeded`, `run.failed` |
| Approvals | `approval.requested`, `approval.approved`, `approval.denied`, `approval.expired` |
| DLQ | `dlq.created`, `dlq.replayed`, `dlq.dismissed` |
| Artifacts | `receipt.available`, `replay_fixture.available` |

`job_validate` enforces the required fields and the cross-field invariants that
matter for hosts: approval waits require `approval.request_id`, DLQ events
require `dlq.entry_id`, receipt events require `receipt.receipt_id`, replay
fixture events require `replay_fixture.fixture_id`, and scheduled-job events
must carry `schedule`.

## Schedule boundary

Dashboard job events do not parse or reinterpret schedule expressions. Cron
triggers continue to use the cron connector's named-timezone and catch-up
semantics, while business-day and timezone logic belongs to `std/calendar`.
The `schedule` block preserves those source fields (`expression`, `timezone`,
`business_calendar`, `next_fire_at`, `catchup_mode`) so hosts can display them
without reimplementing Harn's scheduler.

## Host boundary

Harn owns normalized job events, workflow/run identifiers, approvals, DLQ
state, receipts, and replay fixture links. Hosts own rendering, notification
policy, operator action UX, and concrete mutations such as replaying or
dismissing a DLQ item. A host may cache the latest event per `job_id`, but it
should treat EventLog order plus `source_timestamp` as the audit record.

## Static fixtures

`job_fixture_feed()` returns a deterministic fixture stream for local demos and
tests. The checked-in JSON fixture at
`examples/jobs/fixtures/dashboard_job_events.json` covers queued, running,
approval, DLQ, success, failure, receipt, and replay paths so Burin Home or any
other host can render a Jobs view without a live orchestrator.
