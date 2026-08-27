# Session recap reference

Session recaps are deterministic, read-only projections of canonical session
events. A recap groups visible prompts, assistant text, tool exchanges, plans,
progress, and terminal facts by prompt turn and agent-loop iteration. Building a
recap doesn't call a model or write a derived event.

## Query inputs

Every query requires `sessionId`. The remaining fields are optional:

| Field | Type | Meaning |
|---|---|---|
| `runId` | string | Keep events from one run. |
| `turnId` | string | Keep events from one prompt turn. |
| `fromEventId` | unsigned integer | Start the bounded read at this source event. |
| `limit` | unsigned integer | Request a source-event bound. Harn defaults to 4,096 and clamps the effective bound into 1 through 32,768. |

The HTTP adapter exposes `GET /sessions/{sessionId}/recap`. The ACP adapter
exposes `harn.session_recap.query` with the same fields. Both accept snake-case
aliases for query fields.

## Result states

`state: "available"` carries a `snapshot`. `state: "unavailable"` carries one
of these reasons:

| Reason | Meaning |
|---|---|
| `journal_unavailable` | The canonical session journal isn't available. |
| `session_missing` | No persisted session has the requested ID. |
| `projection_failed` | Harn couldn't derive the deterministic projection. |
| `admission_terminal` | The owning runtime couldn't admit the read. |

An available snapshot can contain zero matching facts. Check
`coverage.scanned`, `coverage.matched`, `coverage.pending`, and
`coverage.unassigned`; don't infer success from an empty `turns` array.

## Snapshot contract

`schemaVersion` is `1`. `source.events` identifies each contributing canonical
event by ID and record hash. `contentHash` binds the source content.
`projectionHash` binds Harn's schema version, session ID, query, cursor,
coverage, content hash, and projected turns. Public text is redacted through
Harn's active redaction policy before it reaches the snapshot.

`projectionHash` deliberately excludes `extensions`. An extension is decorative
until it carries and verifies its own binding to the snapshot's
`projectionHash` or `contentHash`.

The schema-v1 write contract is closed. A writer must reject unknown fields in
the availability envelope or snapshot. Forward-compatible data belongs in the
`extensions` object, which typed bindings preserve when they decode and
re-encode a snapshot.

The authoritative schema is `schemas/session-recap-v1.schema.json`. Generated
Swift, TypeScript, Rust, Python, and Go types ship in the
[`spec/protocol-artifacts`](https://github.com/burin-labs/harn/tree/main/spec/protocol-artifacts)
directory.
