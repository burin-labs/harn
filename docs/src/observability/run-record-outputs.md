# Run-record observability outputs

Harn persists the projections it computes while an agent runs. Observability
consumers should read these fields instead of parsing assistant text or
reconstructing spans from cumulative usage totals.

## Execution evidence

Each run record carries one `evidence` object:

```json
{
  "evidence": {
    "schema_version": 1,
    "execution_id": "hxe-0199...",
    "trace_spans": [],
    "flight_recording": null,
    "gaps": []
  }
}
```

`execution_id` is the owner used by the run record, local spans,
OpenTelemetry, and an optional flight artifact. `gaps` names requested evidence
that Harn couldn't persist. Consumers must treat a non-empty list as partial
evidence rather than silently accepting the record as complete.

Historical records and session-only projections can carry `execution_id: null`.
They include an `execution_identity` gap explaining why the VM owner cannot be
recovered; Harn does not relabel a run or session ID as an execution ID.

Plain `harn run` executions use the same execution identity for the record id.
Workflow records retain their workflow identity and carry the execution owner
inside `evidence.execution_id`.

## Durable agent-event correlation

Agent events written inside a VM execution carry the same `execution_id`.
JSONL tapes store it on each event envelope. SQLite event-log records store it
in both the payload and the indexed headers, so readers can join by the typed
field without parsing event bodies. Events emitted outside a VM scope use
`null`; Harn does not substitute a session ID.

## Span tree

Workflow run records expose completed spans in `evidence.trace_spans`. Keeping
the span tree inside the same evidence object as its execution identity,
flight artifact, and gaps prevents independent observability schemas from
drifting. Each span has a stable `span_id` and optional `parent_span_id`;
joining those fields produces the authoritative tree. LLM spans also expose
`ttft_ms` when Harn observed a first response token.

```json
{
  "evidence": {
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
        "metadata": {"harn.execution.id": "hxe-0199..."}
      }
    ]
  }
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
