# Orchestrator backpressure

Harn applies backpressure at the HTTP ingest edge, the durable trigger queue,
and dispatcher destinations. The goal is to slow new work before one noisy
source, one slow handler, or one unhealthy peer can starve the rest of the
orchestrator.

## HTTP ingest

`harn orchestrator serve` uses token buckets before webhook normalization:

- one global bucket for all incoming trigger requests
- one per-source bucket keyed by provider id

When either bucket is empty, the listener returns `503 Service Unavailable`
with a `Retry-After` header. Providers can retry normally; requests that were
accepted before saturation have already been appended to the durable pending
log.

The default limits are intentionally permissive for local development:

- global capacity: 4096 requests
- per-source capacity: 1024 requests
- refill: 1024 requests per second

Operators can tune the process with:

```bash
HARN_ORCHESTRATOR_INGEST_GLOBAL_CAPACITY=4096
HARN_ORCHESTRATOR_INGEST_PER_SOURCE_CAPACITY=1024
HARN_ORCHESTRATOR_INGEST_REFILL_PER_SEC=1024
```

## Dispatch queue

The EventLog is the durable queue. The long-running inbox pump admits only a
bounded number of outstanding dispatch tasks, so a full pump stops reading new
inbox envelopes and leaves its cursor unacked until capacity is available.

Manifest flow-control gates provide per-trigger controls:

- `concurrency` limits handler slots and queues waiters by optional priority
- `throttle` delays excess work until the window has capacity
- `rate_limit` skips excess work for a window
- `batch`, `debounce`, and `singleton` collapse or suppress redundant work

## Destination circuits

The dispatcher keeps a process-local circuit per handler destination, keyed by
handler kind and target URI. Five consecutive retryable failures open the
circuit for 60 seconds. While open, new events for that destination fail fast
into the trigger DLQ instead of starting another retry spiral.

After the backoff period, the next request is treated as a half-open probe. A
successful probe closes the circuit; a failed probe opens it again for another
backoff window.

## Metrics

All backpressure decisions increment:

```text
harn_backpressure_events_total{dimension, action}
```

Current dimensions include `ingest` and `circuit`. Pair this with
`harn_trigger_accepted_to_dlq_seconds`, `harn_trigger_oldest_pending_age_seconds`,
and `harn_orchestrator_pump_outstanding` to alert on sustained degradation.
