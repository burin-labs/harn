# Observability

`std/observability` is the friendly API for user-space spans, logs, metrics,
and structured events. Configure routing once at runtime, then emit observations
without choosing a wire format at every call site.

Use the short alias `import { obs } from "observability"`, or the explicit
stdlib path `import { obs } from "std/observability"`.

```harn
import { obs } from "std/observability"

pipeline default() {
  let o = obs()
  o.configure({backend: o.Backend.auto})

  return o.span("plan_review", {pr_number: 1915}, { ->
    o.log("starting review", "info", {phase: "start"})
    let result = {duration_ms: 42, status: "ok"}
    o.metric("review_duration_ms", result.duration_ms, {unit: "ms"})
    return result
  })
}
```

## Backends

`o.Backend.auto` selects a backend from the process environment:

1. `OTEL_EXPORTER_OTLP_ENDPOINT` or `HARN_OTEL_ENDPOINT`: OTel OTLP-shaped payloads.
2. `SPLUNK_HEC_TOKEN`: Splunk HEC-shaped JSON.
3. `HONEYCOMB_API_KEY`: Honeycomb-style flat events.
4. Otherwise: human-readable `pretty_stderr`.

Explicit backends:

```harn
import { obs } from "std/observability"

let B = obs().Backend

obs().configure({backend: B.otel(env("OTEL_EXPORTER_OTLP_ENDPOINT"))})
obs().configure({backend: B.splunk_hec(env("SPLUNK_HEC_ENDPOINT"), env("SPLUNK_HEC_TOKEN"))})
obs().configure({backend: B.honeycomb(env("HONEYCOMB_API_KEY"), "harn")})
obs().configure({backend: B.pretty_stderr})
obs().configure({backend: B.compose([B.otel("http://collector:4318"), B.pretty_stderr])})
```

## Routing

Routes dispatch by `kind`, `level`, or `default` to a named backend:

```harn
import { obs } from "std/observability"

let o = obs()
let B = o.Backend

o.configure({
  backends: {
    otel: B.otel("http://collector:4318"),
    splunk: B.splunk_hec("https://splunk.example/services/collector", env("SPLUNK_HEC_TOKEN")),
    honeycomb: B.honeycomb(env("HONEYCOMB_API_KEY"), "harn"),
  },
  routes: [
    {level: "error", backend: "splunk"},
    {kind: "metric", backend: "honeycomb"},
    {default: "otel"},
  ],
})
```

Span attributes merge into log and metric fields while the span is active, so
correlated events carry the same `trace_id`, `span_id`, and attribute bag.

## Processors

Processors run after span/request/tenant enrichment and before backend routing.
Use the stock redaction processor when logs or spans can carry credentials:

```harn
import { obs } from "std/observability"

let o = obs()
o.configure({
  backend: o.Backend.otel("http://collector:4318"),
  processors: [o.Processor.redaction],
})
```

`redaction` applies the active runtime redaction policy to the entire event, so
OTLP, Splunk, Honeycomb, pretty, and test backends all receive the same scrubbed
payload.
