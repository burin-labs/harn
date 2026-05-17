# Tool-call spans

`with_telemetry` (from `std/llm/tool_middleware`) emits a standardized
span for every tool dispatch funneled through the middleware stack.
The span shape is the contract that downstream observability backends
(Langfuse, OpenTelemetry, custom dashboards) bind to — fields are
additive, and a sink that doesn't recognize a field MUST ignore it.

## Schema

```text
{
  // Identity
  name:           "tool_call.<tool_name>",
  kind:           "tool_call",
  span_id:        <tool_call_id>,
  trace_id:       <session_id | "tool_call:" + call_id>,
  parent_span_id: nil | string,

  // Timing
  start_time_ms, end_time_ms, duration_ms:  int (wall-clock ms),
  start_time_iso, end_time_iso:             string (RFC3339, optional),

  // Outcome
  status: "ok" | "error" | "dry_run" | "timeout" | "rate_limited"
        | "consent_denied" | "schema_violation" | "redacted" | …,

  // Attributes (Langfuse → metadata, OTel → span attributes)
  attributes: {
    tool_name, tool_call_id, executor, status, ok,
    args_hash:       sha256 of canonical-JSON tool args,
    iteration:       agent_loop turn index,
    session_id:      agent_loop session id,
    persona?:        caller persona (when provided),
    mode?:           caller mode (when provided),
    error?:          string,
    error_category?: string,
    rendered_result_size?: int,
    // OTel-compatible mirrors:
    "gen_ai.tool.name":     tool_name,
    "gen_ai.tool.call.id":  tool_call_id,
    "gen_ai.tool.executor": executor,
  },

  // Point-in-time events
  events: [
    {name: "tool_call.dispatched",        time_ms: int},
    {name: "tool_call.result_returned",   time_ms: int},
    {name: "tool_call.scope_violation",   time_ms: int, attributes: {...}},  // when applicable
  ],

  // Child spans (one per audit-trail layer — e.g. with_consent, with_rate_limit)
  child_spans: [
    {name: "tool_call.<layer>", status, duration_ms?, started_at?, ended_at?, error?, attributes?},
    …
  ],

  // Raw snapshots — kept for sinks that need full fidelity
  call:   <middleware envelope>,
  result: <dispatch-result dict>,
}
```

`span_id` reuses the `tool_call_id` so traces, audit-log records, and
agent-emitted `tool_call_audit` events can be joined without an extra
correlation key.

## Composing a sink

```harn,ignore
import { with_telemetry } from "std/llm/tool_middleware"

let captain_caller = compose_tool_callers([
  with_audit_log(receipts_sink),
  with_telemetry({sink: "langfuse", project: "harn-dev"}),
  // ...rest of the captain stack
])
```

`with_telemetry` accepts:

- a callable `fn(span)` — invoked once per dispatch with the standardized span;
- a built-in name (`"langfuse"`, `"otel"`, `"stderr"`, `"noop"`);
- a dict `{sink: callable | name, ...opts}` for one sink with options;
- a dict `{sinks: [callable | name | dict, ...], ...opts}` to fan out.

Recognized top-level `opts` (passed through to sinks and used to
decorate the span):

| Field                | Used by         | Description                                          |
| -------------------- | --------------- | ---------------------------------------------------- |
| `persona`            | span attributes | Identity tag — e.g. `"merge_captain"`               |
| `mode`               | span attributes | `"preview"` / `"apply"` / custom mode label         |
| `parent_span_id`     | span identity   | Set when nested under an agent-turn span             |
| `trace_id_override`  | span identity   | Replace the default session-derived trace id        |
| `extra_attributes`   | span attributes | Free-form additional attributes (merged in)         |
| `base_url`           | Langfuse        | Defaults to `LANGFUSE_BASE_URL` env                  |
| `public_key`         | Langfuse        | Defaults to `LANGFUSE_PUBLIC_KEY` env                |
| `secret_key`         | Langfuse        | Defaults to `LANGFUSE_SECRET_KEY` env                |
| `project`            | Langfuse        | Sets `metadata.project` on each observation         |
| `environment`        | Langfuse        | `metadata.environment` tag (dev/staging/prod/…)     |
| `release`            | Langfuse        | `metadata.release` tag                               |
| `timeout_ms`         | Langfuse        | HTTP timeout per POST (default `5000`)              |
| `on_error`           | Langfuse        | `fn(message, span)` — telemetry failures are otherwise swallowed |
| `prefix`             | stderr / OTel   | String prefix on each stderr line                   |

## Built-in sinks

### `"langfuse"`

POSTs each span to `${LANGFUSE_BASE_URL}/api/public/ingestion` as an
`observation-create` event with `type: "SPAN"`. The tool arguments
land in `input`, the dispatch result in `output`, and the rest of the
span (executor, args hash, child spans, events) lands under
`metadata`. Authentication is Basic-auth with
`LANGFUSE_PUBLIC_KEY:LANGFUSE_SECRET_KEY`.

Each call POSTs synchronously, so a burst of tool calls also produces a
burst of HTTP requests. For high-throughput workloads, fan out via
`{sinks: [...]}` and have one sink batch on its own cadence, or compose
`with_idempotency` upstream to deduplicate.

### `"otel"` *(stubbed)*

Formats spans as OpenTelemetry-shape JSON
(`start_time_unix_nano` / `end_time_unix_nano`, `status: {code, message}`)
and writes them to stderr by default. Passing `{sink: my_emitter}` redirects
each formatted record to a custom callable.

The full OTLP HTTP export path is wired separately via the
[`HARN_OTEL_ENDPOINT`](./orchestrator/observability.md) env var on the
orchestrator subscriber. Tool-call spans will flow through that same
pipeline in a follow-up.

### `"stderr"`

Dev-friendly: prints one JSON line per span to stderr. Pipe to `jq` or
`grep` while iterating on a script.

### `"noop"`

Discards spans. Useful when telemetry is conditionally disabled (e.g.
`with_telemetry(env_bool("HARN_TELEMETRY", true) ? "langfuse" : "noop")`).

## Custom sinks

A sink is `fn(span) -> any`. Exceptions inside a sink are swallowed by
the middleware so a flaky exporter never breaks the agent loop. For
backends not covered by the built-ins, write a sink that POSTs to the
backend's API and pass it directly:

```harn,ignore
let prom_sink = { span ->
  http_request("POST", "http://prom.local/observe", {
    headers: {"content-type": "application/json"},
    body: json_stringify({
      tool: span.attributes.tool_name,
      duration_ms: span.duration_ms,
      status: span.status,
    }),
  })
}
agent_loop(task, system, {
  tool_caller: compose_tool_callers([with_telemetry(prom_sink)]),
})
```

## Joining with audit logs and receipts

Every tool dispatch already emits a `tool_call_audit` event with the
same `tool_call_id`. To stitch telemetry to the audit ledger:

- `tool_call_audit.tool_call_id == span.span_id`
- `tool_call_audit.audit.layers` mirrors `span.child_spans` 1:1 (the
  span builder derives child spans from `result.audit.layers`)
- `session_id` is shared, so all spans + audits + receipts for one
  agent loop session correlate by `attributes.session_id`.

## Stability

The span schema is additive: new fields can appear in any release, and
sinks MUST ignore unknown fields. Existing fields are stable across
patch releases. Anything that would remove or rename a field will land
through a minor-version bump with a migration note in `CHANGELOG.md`.
