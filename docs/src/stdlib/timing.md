# Timing

`std/timing` is the scoped-duration primitive for Harn scripts. It replaces
hand-rolled `let started_ms = harness.clock.now_ms()` subtraction with a
first-class observability span that carries trace identifiers, monotonic
duration, wall-clock start/end, status, attributes, and sub-phase events.

Use a timing segment when **duration matters**. Use a metric when you want a
pre-aggregated numeric series. Use a span attribute when the value belongs on
the current span instead of a new one.

```harn
import { timed } from "std/timing"

pipeline default() {
  let outcome = timed("benchmark.agent_loop", {case_id: "python-add"}, { ->
    return repair_agent(task)
  })
  __io_println("duration_ms=${outcome.timing.duration_ms}")
  return outcome.result
}
```

`timed` is the docs-forward shape because the callback cannot forget to close.
The imperative pair is still available for flows that cross callbacks,
branches, or async-ish lifecycle boundaries:

```harn,ignore
import { start_timing, timing_event, end_timing } from "std/timing"

pipeline default() {
  let timing = start_timing("provider.preflight", {provider: "ollama"})
  timing_event(timing, "credentials.checked", {available: true})
  timing_event(timing, "model.warm", {cache: "hit"})
  let result = preflight()
  let finished = end_timing(timing, {status: "ok"})
  return {result: result, duration_ms: finished.duration_ms}
}
```

## TimingSegment shape

The dict returned from `timed(...).timing` and `end_timing(...)` looks like:

```text
{
  name: "benchmark.agent_loop",
  trace_id: "trace_...",
  span_id: 42,
  parent_span_id: 41,                // nil at the trace root
  kind: "user_timing",
  started_at_ms: 1700000000123,      // wall-clock millis (UNIX epoch)
  monotonic_started_ms: 12345,       // for elapsed checks via elapsed_ms(...)
  ended_at_ms: 1700000000188,
  duration_ms: 65,
  status: "ok",                      // "ok" | "error" | custom
  attributes: {case_id: "python-add"},
  metadata: {case_id: "python-add", status: "ok"},
  events: [                          // present when timing_event(...) ran
    {name: "checkpoint", offset_ms: 4, time_unix_ms: 0, attributes: {}}
  ]
}
```

`duration_ms` is always derived from the monotonic clock, so it stays
deterministic under `mock_time(...)` / `advance_time(...)`. Wall-clock fields
(`started_at_ms`, `ended_at_ms`) honor the mock as well, so testbench fixtures
that pin wall time align with timing output.

## Sub-phase events

`timing_event(handle, name, attrs?)` records a sub-phase annotation on the open
span without allocating a child span for every marker. Events ride along with
the parent span — they show up in `timing.events` after `end_timing`, in
`trace_spans()` output, and in OTel exports as native span events. Returns
`true` when the event was attached, `false` when the handle is already closed.

## Duplicate close is idempotent

Calling `end_timing(handle)` twice returns the cached segment from the first
close instead of corrupting the active span stack. This lets defensive
wrappers call `end_timing` without re-checking handle state, and means a
callback path that closes its handle still produces a consistent return value
if a follow-up `finally`-style block calls `end_timing` again.

## Errors are captured automatically

When `timed(...)` propagates an exception, it closes the span with
`{status: "error", error: "<message>"}` before re-throwing. Imperative callers
should pass the same shape explicitly:

```harn,ignore
import { start_timing, end_timing } from "std/timing"

let timing = start_timing("risky")
try {
  risky_work()
  end_timing(timing, {status: "ok"})
} catch (e) {
  end_timing(timing, {status: "error", error: to_string(e)})
  throw e
}
```

## Profile and OTel export

`harn run --profile-json` includes user timing spans under the
`user_timing` bucket alongside the existing pipeline / llm_call / tool_call
categories. OTel exporters surface them as INTERNAL spans named after the
`name` argument, so they do not collide with the GenAI semconv mappings used
by LLM and tool spans. Span attributes flow through the same redaction policy
used for receipts, OTel attributes, and OAuth scopes.

## When to skip a timing span

- Tight inner loops (per-iteration spans are pure overhead).
- Boundary points where `std/observability::log` or `metric` is enough — pick
  a metric when you want pre-aggregated numbers, an attribute when the value
  belongs on the current span, and a timing span when the duration of a
  discrete piece of work matters.
