# Unified Observability API

Use `std/observability` when Harn code wants to record "something happened"
without caring whether the configured backend represents it as a log, span,
metric, or event.

```harn
import { obs } from "std/observability"

pipeline default() {
  const o = obs()
  o.configure({backend: o.Backend.auto})

  const span = o.start_span("sync", {tenant: "acme"})
  o.log_in_span(span, "queued", "info", {items: 3})
  o.end_span(span)
}
```

For scoped work, prefer callback form:

```harn
o.span("sync", {tenant: "acme"}, { ->
  o.log("started")
  o.metric("items_synced", 3)
})
```

Configure processors when events need a shared transform before export:

```harn
o.configure({
  backend: o.Backend.auto,
  processors: [o.Processor.redaction],
})
```

The stock `redaction` processor applies the active runtime redaction policy
before OTLP, Splunk, Honeycomb, pretty, or test payloads are formatted.
