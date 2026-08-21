# Run-record observability outputs

Harn persists the projections it computes while an agent runs. Observability
consumers should read these fields instead of parsing assistant text or
reconstructing spans from cumulative usage totals.

## Span tree

Workflow run records expose completed spans in `trace_spans`. Each span has a
stable `span_id` and optional `parent_span_id`; joining those fields produces
the authoritative tree. LLM spans also expose `ttft_ms` when Harn observed a
first response token.

```json
{
  "trace_spans": [
    {
      "trace_id": "trace_...",
      "span_id": 8,
      "parent_span_id": 3,
      "kind": "llm_call",
      "name": "llm_call",
      "start_ms": 120,
      "duration_ms": 900,
      "ttft_ms": 125,
      "metadata": {}
    }
  ]
}
```

`start_ms` is relative to the run's tracing epoch. `duration_ms` and `ttft_ms`
are monotonic durations. Root spans set `parent_span_id` to `null`; spans
without an observed first token omit `ttft_ms`.

Run records written before this schema used `parent_id`. Harn still accepts
that name when loading historical records, while newly persisted records use
`parent_span_id`.

## Per-turn parsed output

Every model response is appended as a `provider_call_response` event on the
`agent.transcript.llm` topic in `.harn/events.sqlite`. In addition to the raw
response `text`, the event includes:

- `parsed_tool_calls` or `parsed_tool_calls_ref`: Harn's normalized tool-call
  parse. When a non-empty view is structurally identical to the provider-native
  `tool_calls` array, the response stores `parsed_tool_calls_ref: "tool_calls"`
  instead of serializing the array twice. Empty and distinct views stay in the
  smaller inline `parsed_tool_calls` representation, including calls parsed
  from Harn's text tool protocol and its legacy `name({...})` form. Rust
  consumers should use
  `harn_vm::llm::response_tool_calls::resolve`; absence without a reference is
  a historical/partial row, not an alias.
- `loop_state`: a decoded object when the response contains a complete
  `## LOOP_STATE` / `## END_LOOP_STATE` block. Booleans, numbers, `null`, and
  `nil` become JSON primitives; other values remain strings. The field is
  `null` when no complete block exists.
- `tool_calls`: the provider-native receipt only. It can be empty even when an
  inline `parsed_tool_calls` projection contains text-protocol calls.

These projections are redacted by the same transcript policy as `text` before
they reach SQLite. Correlate a response with its request and related records by
`call_id`; `iteration` identifies its agent-loop turn and `span_id` links it to
the run-record span tree.

Consumers that need the whole timeline should read the event-log topic in
event order. They do not need the optional `llm_transcript.jsonl` debug
sidecar.
