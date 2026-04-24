# Monitor stdlib

`std/monitors` provides `wait_for(...)` for waiting on external state while
preserving deterministic replay records.

```harn,ignore
import { wait_for } from "std/monitors"

let result = wait_for({
  timeout: 30m,
  poll_interval: 10s,
  source: {
    label: "deploy",
    poll: { ctx -> github_status(ctx.wait_id) },
    prefers_push: true,
    push_filter: { event -> event.payload.event.kind == "deployment_status" },
  },
  condition: { state -> state.status == "success" },
})
```

## Source shape

A monitor source is a dict with:

- `poll`: closure returning the latest authoritative state. It receives a
  context dict with `wait_id`, `poll_count`, and `last_push_event`.
- `label`: optional human-readable source label stored in the wait record.
- `prefers_push`: optional bool that enables push wakeups when true.
- `push_filter`: optional closure that receives trigger inbox event-log entries
  and returns true when the event should wake the monitor.

When a push filter matches, Harn wakes the monitor and polls again. Pollers can
use `ctx.last_push_event` as a wakeup hint or as the state source for pure
webhook arrivals. For externally queryable state, prefer treating the webhook as
a wakeup hint and repolling the source of truth.

## Result shape

`wait_for(options)` returns a monitor wait record:

- `status`: `"matched"`, `"timed_out"`, or `"interrupted"`.
- `state`: the last polled state.
- `condition_value`: the last condition result.
- `poll_count`: number of poll calls.
- `push_wake_count`: number of matching push wakeups.
- `wait_id`, `trace_id`, timestamps, source label, and timeout reason.

Inside trigger handlers, monitor waits suspend the dispatcher wait lease, so
singleton and concurrency flow-control slots are released while the handler is
blocked. Terminal results are recorded on `monitor.waits`; replay resolves from
that record instead of polling live systems.
