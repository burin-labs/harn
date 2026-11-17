# Monitor stdlib

`std/monitors` provides `wait_for(...)` for waiting on external state while
preserving deterministic replay records.

```harn
import { MonitorSource, wait_for } from "std/monitors"

type ReadyState = {ready: bool}

fn main(harness: Harness) {
  const source: MonitorSource<ReadyState> = {
    label: "ready",
    poll: { _ -> {ready: true} },
  }
  const result = wait_for(harness.runtime, {
    timeout: 1s,
    source: source,
    condition: { state -> state.ready },
  })
  if result.state != nil {
    harness.stdio.println(result.state.ready)
  }
}
```

## Source shape

`MonitorSource<T>` describes a source whose poll returns state type `T`:

- `poll`: closure returning the latest authoritative state. It receives a
  `MonitorPollContext` with `wait_id: string`, `poll_count: int`, and
  `last_push_event: unknown`. The first poll receives `nil` for the push event.
- `label`: optional human-readable source label stored in the wait record.
- `prefers_push`: optional bool that enables push wakeups when true.
- `push_filter`: optional closure that receives trigger inbox event-log entries
  and returns true when the event should wake the monitor.

When a push filter matches, Harn wakes the monitor and polls again. Pollers can
use `ctx.last_push_event` as a wakeup hint or as the state source for pure
webhook arrivals. For externally queryable state, prefer treating the webhook as
a wakeup hint and repolling the source of truth.

`monitor_event(log_event)` reads the event carried by a log record's
`payload.event`. It accepts dictionary and nominal record containers, including
partial events. Scalar outer records are refused; missing or scalar inner
containers return `nil`. Its `MonitorEventReference` exposes optional `provider`,
`kind`, and `provider_payload` fields as `unknown`. Decode provider-specific
values before using them as strings, lists, or records. Extra fields remain intact.

GitHub, Linear, and Slack source factories return `MonitorSource<T>` with a
named state type. Their states preserve raw provider records and extensions as
`unknown`; normalized fields such as Slack's `matched` remain typed. Slack's
first poll returns `matched: false` without a push event. Linear's first poll
queries the configured connector when no push event is available.

| Module | Factory | State type |
|---|---|---|
| `std/connectors/github` | `deployment_status_source` | `GitHubDeploymentState` |
| `std/connectors/github` | `check_run_source` | `GitHubCheckRunState` |
| `std/connectors/github` | `pull_request_merged_source` | `GitHubPullRequestState` |
| `std/connectors/linear` | `issue_state_source` | `LinearIssueState` |
| `std/connectors/slack` | `message_source` | `SlackMessageState` |
| `std/connectors/slack` | `reaction_source` | `SlackReactionState` |

`monitor_wait_options(options, source, condition)` attaches a typed source after
validating the timing values. `timeout` requires a duration. An omitted
`poll_interval` defaults to 10 seconds; explicit `nil`, non-duration values,
and zero intervals are refused.

## Result shape

`wait_for(runtime, options: MonitorWaitOptions<T>)` returns `MonitorWaitResult<T>`:

- `status`: `"matched"`, `"timed_out"`, or `"interrupted"`.
- `state: T?`: the last polled state, or `nil` if interrupted before a poll.
- `condition_value: unknown`: the last condition result.
- `poll_count`: number of poll calls.
- `push_wake_count`: number of matching push wakeups.
- `wait_id`, `trace_id`, timestamps, source label, and timeout reason.

A zero timeout still polls once. If that poll doesn't match, the result keeps
the measured state and reports `timed_out`.

Inside trigger handlers, monitor waits suspend the dispatcher wait lease, so
singleton and concurrency flow-control slots are released while the handler is
blocked. Terminal results are recorded on `monitor.waits`; replay resolves from
that record instead of polling live systems.
