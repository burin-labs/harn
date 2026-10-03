# Session health reference

The agent loop emits `session_health` after each model iteration and when it
publishes a terminal outcome. A tool call resolved after that snapshot emits an
updated fact, including calls abandoned during closeout. The version 1 fact contains `session_id`,
`iteration`, `turn`, `rolling`, and `heuristics`. It observes execution and does
not change loop policy. A health measurement is not an evaluation verdict.

`turn` contains the current model iteration's measurements. `rolling` covers
observations retained by the runtime session. Resetting the session transcript
resets these observations. Loading historical messages does not invent missing
execution telemetry. An iteration is one model round trip, not an ACP prompt
turn; see the [glossary](concepts/glossary.md).

## Measurements

Every measurement is nullable. `null` means unmeasured. A rate contains its
`numerator`, `denominator`, and `value`; no qualifying observations produce
`null`, not a zero rate.

| Field | Measurement |
|---|---|
| `tool_call_success_rate` | Completed successful calls divided by completed calls. Streaming parse candidates are excluded. |
| `nonzero_command_exit_rate` | Nonzero producer-reported exits divided by reported exits. An absent exit code is excluded. |
| `edits_since_verification` | Applied tool mutations since the most recent completed declared verification, or since observation began. A verification resets the count even if it fails. Before either an edit or verification, the value is `null`. |
| `diagnostic_trend` | Change between complete diagnostic collections from declared verification. Lower cardinality is `improving`, higher is `regressing`, and equal is `flat`. Returning to the set seen two observations earlier, after a different set, is `oscillating`. One collection is insufficient to measure a trend. |
| `turn_wall_time` | Mean elapsed milliseconds and sample count. `expected` is the mean of prior measured iterations, with source `prior_measured_turns` and its sample count. `actual_to_expected` is absent until a positive expectation exists. |
| `prose_to_tool_balance` | Emitted prose character count, completed tool-call count, and characters per call. Without a prose measurement the field is `null`; without a tool call the ratio is `null`. |
| `last_stop_class` | The most recently observed canonical terminal classification. |

Edit count, diagnostic trend, and stop class are session gauges shared by the
two views. Counts and rates use their respective iteration or rolling
population. Repeated terminal updates for one call do not increase the
population twice.

Command outcomes come from normalized producer fields. Diagnostic measurement
requires an explicit complete collection, including an empty collection, and
excludes advisory evidence. Rendered tool feedback and assistant message
content are not analyzed. Diagnostic telemetry retains a set fingerprint and
count, without diagnostic messages or paths.

`heuristics` is a separate, currently empty object. Consumers that use health
to steer execution must supply an explicit policy; the runtime supplies no
default steering rule.

## Transports and schema

Harn event subscribers receive `SessionHealth` with its `fact`. ACP emits
`_harn/agentEvent` with kind `session_health`; its payload is the fact. A2A task
streams expose `harn_session_health` with a `health` field containing the same
fact. Durable agent-event sinks retain the event for offline readers.

The generated contract is
[`session-health.schema.json`](https://github.com/burin-labs/harn/blob/main/spec/protocol-artifacts/schemas/session-health.schema.json).
Its schema version is `1`. The protocol artifact generator derives the schema
from the runtime's Rust types; see [generated protocol artifacts](protocol-artifacts.md).
