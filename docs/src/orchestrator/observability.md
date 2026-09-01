# Orchestrator observability

`harn orchestrator serve` exposes metrics, structured logs, and OpenTelemetry
spans for the trigger pipeline.

## Metrics

The HTTP listener serves Prometheus exposition at `/metrics` on the same bind
address as trigger ingestion:

```bash
curl http://127.0.0.1:8080/metrics
```

The endpoint includes HTTP request counters and histograms, trigger pipeline
metrics, EventLog append timings, budget gauges, A2A hop timings, worker queue
depth and claim age, backpressure counters, and LLM call/cache counters. Trigger labels use the
manifest trigger id, provider, handler kind, and outcome where applicable.

Webhook and trigger latency metrics use low-cardinality labels:
`provider`, `trigger_id`, `binding_key`, `tenant_id`, and `status`. When an
event has no tenant, `tenant_id` is `none`.

Lifecycle histograms:

- `harn_trigger_webhook_accepted_to_normalized_seconds`
- `harn_trigger_webhook_accepted_to_queue_append_seconds`
- `harn_trigger_queue_age_at_dispatch_admission_seconds`
- `harn_trigger_queue_age_at_dispatch_start_seconds`
- `harn_trigger_dispatch_runtime_seconds`
- `harn_trigger_retry_delay_seconds`
- `harn_trigger_accepted_to_dlq_seconds`

Oldest pending age is exposed as `harn_trigger_oldest_pending_age_seconds` with
the same trigger/provider/tenant labels, excluding `status`.

Backpressure decisions are exposed as
`harn_backpressure_events_total{dimension, action}`. The `ingest` dimension
tracks HTTP token-bucket admission/rejection; the `circuit` dimension tracks
destination circuit transitions and fail-fast DLQ moves.

### LLM spend certainty

LLM metrics distinguish logical operations from physical provider requests so
schema repair, transport retry, and structured-output retry traffic cannot be
reported as one call or as free:

- `harn_llm_calls_total{provider,model,outcome}` counts logical operations.
- `harn_llm_provider_requests_total{provider,model}` counts physical provider
  requests represented by their transaction ledgers.
- `harn_llm_cost_usd_total{provider,model}` is the cumulative known USD lower
  bound across those requests.
- `harn_llm_unpriced_requests_total{provider,model}` counts requests whose USD
  cost is not known. A window is exact only when this counter has no increase.
- `harn_llm_usage_unknown_requests_total{provider,model}` counts requests whose
  token usage is unknown, including terminal attempts that produced no usable
  response.

For example, this query returns the routes whose spend was not exact during the
last hour:

```promql
sum by (provider, model) (
  increase(harn_llm_unpriced_requests_total[1h])
) > 0
```

Do not replace a missing price or terminal request receipt with zero. Combine
the known-cost increase with the unpriced-request increase when displaying a
spend total, and label it as a lower bound whenever the latter is nonzero.

Example webhook-to-dispatch latency SLO queries:

```promql
histogram_quantile(
  0.50,
  sum by (le, provider, trigger_id) (
    rate(harn_trigger_queue_age_at_dispatch_start_seconds_bucket[5m])
  )
)
```

```promql
histogram_quantile(
  0.95,
  sum by (le, provider, trigger_id) (
    rate(harn_trigger_queue_age_at_dispatch_start_seconds_bucket[5m])
  )
)
```

```promql
histogram_quantile(
  0.99,
  sum by (le, provider, trigger_id) (
    rate(harn_trigger_queue_age_at_dispatch_start_seconds_bucket[5m])
  )
)
```

Example alert for queued work older than 30 seconds:

```promql
max by (provider, trigger_id, tenant_id) (
  harn_trigger_oldest_pending_age_seconds
) > 30
```

Example DLQ latency alert:

```promql
histogram_quantile(
  0.95,
  sum by (le, provider, trigger_id) (
    rate(harn_trigger_accepted_to_dlq_seconds_bucket[10m])
  )
) > 300
```

`harn orchestrator inspect` also surfaces the persisted trigger metric snapshot
for local diagnosis. Its JSON `recent_dispatches` entries include `trace_id`
and `run_path` when Harn persisted a run, so trace viewers can open the stable
run view without querying Harn's event database:

```bash
harn orchestrator inspect --state-dir ./.harn/orchestrator
```

## Logs

By default, process logs keep the compact text format used by existing local
tools. For container-friendly structured logs, run:

```bash
harn orchestrator serve --log-format json
```

`--log-format` accepts `text`, `pretty`, or `json` and can also be configured
with `HARN_ORCHESTRATOR_LOG_FORMAT`. `RUST_LOG` controls filtering, for example:

```bash
RUST_LOG=harn=debug harn orchestrator serve --log-format json
```

JSON logs are newline-delimited and are written to stdout. The same records are
mirrored to `<state-dir>/logs/orchestrator.log`, which rotates to
`orchestrator.log.1` at 10 MiB. Records include the tracing fields emitted at
each boundary, including `trace_id` for request-scoped events.

## OpenTelemetry

Set `HARN_OTEL_ENDPOINT` to enable OTLP HTTP trace export:

```bash
HARN_OTEL_ENDPOINT=http://otel-collector:4318 \
HARN_OTEL_SERVICE_NAME=harn-orchestrator \
harn orchestrator serve
```

Harn also accepts the standard `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`,
and `OTEL_EXPORTER_OTLP_HEADERS` variables. A non-empty Harn-specific variable
wins when both forms are set. The VM event exporter and orchestrator subscriber
use this same configuration path.

The orchestrator exports spans for webhook ingestion, queue append, and
dispatch, attaches the Harn `trace_id` as a span attribute, and propagates W3C
trace context through pending and inbox EventLog topics into dispatch spans.
Dispatcher span events mark dispatch start, handler completion, retry
scheduling, and DLQ movement. A2A dispatches also carry `A2A-Trace-Id` and
`traceparent` headers downstream.

Optional OTLP request headers can be supplied as comma-, semicolon-, or
newline-separated `key=value` entries:

```bash
HARN_OTEL_HEADERS='authorization=Bearer token,x-tenant-id=acme'
```

Set `HARN_OTEL_SAMPLE_RATIO` to downsample root traces at the source before
export. The default is `1.0`, which keeps every trace. Values must be between
`0.0` and `1.0`; sampled parent decisions continue to propagate through child
spans.

```bash
HARN_OTEL_SAMPLE_RATIO=0.1 harn orchestrator serve
```

## Dashboard

An example Grafana dashboard is available at
[`docs/src/orchestrator/observability-dashboard.json`](./observability-dashboard.json).
