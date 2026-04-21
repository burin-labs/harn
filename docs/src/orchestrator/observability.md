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
depth and claim age, and LLM call/cache counters. Trigger labels use the
manifest trigger id, provider, handler kind, and outcome where applicable.

`harn orchestrator inspect` also surfaces the persisted trigger metric snapshot
for local diagnosis:

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

The orchestrator exports spans for webhook ingestion and dispatch, attaches the
Harn `trace_id` as a span attribute, and propagates W3C trace context through
the EventLog into dispatch spans. A2A dispatches also carry `A2A-Trace-Id` and
`traceparent` headers downstream.

Optional OTLP request headers can be supplied as comma-, semicolon-, or
newline-separated `key=value` entries:

```bash
HARN_OTEL_HEADERS='authorization=Bearer token,x-tenant-id=acme'
```

## Dashboard

An example Grafana dashboard is available at
[`docs/src/orchestrator/observability-dashboard.json`](./observability-dashboard.json).
