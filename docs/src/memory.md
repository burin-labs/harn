# Memory

`std/memory` provides durable observations that can be recalled across later
runs without treating transcript history as long-term knowledge.

```harn
import "std/memory"

memory_store("workspace/acme", "alice-profile", {
  text: "Alice prefers Rust examples and concise plans",
}, ["profile", "preference"])

let related = memory_recall("workspace/acme", "rust preference", 3)
let summary = memory_summarize("workspace/acme", {limit: 10})
```

## API

| Function | Returns | Description |
|---|---|---|
| `memory_store(namespace, key, value, tags?, options?)` | `memory_record` | Append an observation |
| `memory_recall(namespace, query, k?, options?)` | `list<memory_record>` | Recall active records ranked by deterministic BM25-style lexical scoring |
| `memory_summarize(namespace, window?, options?)` | `memory_summary` | Build an extractive summary over recent or query-filtered records |
| `memory_forget(namespace, predicate, options?)` | `dict` | Append a tombstone for matching records |

## Storage

The VM-native backend stores append-only JSONL events at
`.harn/memory/<namespace>/events.jsonl` by default. Pass `{root: "path"}` in
the `options` argument to use a different root. Namespaces are relative path
segments; absolute paths and `..` escapes are rejected.

Records contain:

```json
{
  "_type": "memory_record",
  "id": "uuid-v7",
  "namespace": "workspace/acme",
  "key": "alice-profile",
  "value": {"text": "Alice prefers Rust examples"},
  "text": "{\"text\":\"Alice prefers Rust examples\"}",
  "tags": ["profile"],
  "stored_at": "2026-04-29T00:00:00Z",
  "provenance": null
}
```

`memory_store` accepts `options.id`, `options.now`, and `options.provenance`.
These are useful for tests, imports, and replay fixtures.

## Recall And Summary

`memory_recall` is deterministic and local. It tokenizes the record key, tags,
text, and JSON value, then ranks active records with BM25 plus small exact
key/tag boosts. It does not call an embedding service.

`memory_summarize` returns `{_type, namespace, count, text, records}`. `window`
may be `nil`, an integer limit, or a dict with `limit`, `query`, and `tag` or
`tags`. The summary text is an extractive bullet list capped to a bounded size.
Callers that need model-written prose can pass `summary.records` to `llm_call`.

## Forgetting

`memory_forget` is soft-delete. It appends a tombstone event and leaves prior
observations in the log for auditability.

Predicates may be a string substring match, or a dict with any combination of
`id`, `key`, `tag` / `tags`, and `query`. Dict predicates are conjunctive: all
provided fields must match.

## Replay

Memory is separate from transcript history. Runs that recall memory should
persist the recalled records in their run record before deterministic replay;
future host-backed or vector-backed memory stores must preserve that same
boundary instead of silently refreshing memory during replay.
