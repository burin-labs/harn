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
| `memory_open(namespace, options?)` | `memory_open` | Select the recall backend (`bm25`, `vector`, or `hybrid`) for this namespace |
| `memory_store(namespace, key, value, tags?, options?)` | `memory_record` | Append an observation |
| `memory_recall(namespace, query, k?, options?)` | `list<memory_record>` | Recall active records ranked by the namespace backend (override per-call with `options.mode`) |
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

## Recall and summary

`memory_recall` defaults to deterministic, local BM25. It tokenizes the record
key, tags, text, and JSON value, then ranks active records with BM25 plus small
exact key/tag boosts.

Vector and hybrid recall are available via [`memory_open`](#vector-and-hybrid-backends).
When the active backend uses embeddings, recall calls the host’s `memory.embed`
capability (see [Host boundary](host-boundary.md)) and caches the result on
disk so subsequent recalls on the same `(namespace, query, mode, model_hint,
top_k)` are deterministic.

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

## Vector and hybrid backends

`memory_open(namespace, options)` writes an append-only configuration event
that selects the recall backend:

```harn
import "std/memory"

memory_open("workspace/acme", {
  backend: "hybrid",          // "bm25" (default), "vector", or "hybrid"
  embed_model_hint: "voyage-2",
  embed_dim: 1024,
  bm25_weight: 0.4,           // hybrid only
  cosine_weight: 0.6,         // hybrid only
})
```

The latest open event wins, so re-opening a namespace re-keys recall without
rewriting prior records. `memory_recall` accepts a per-call `options.mode`
(`lexical | semantic | hybrid`) that overrides the namespace default for that
query only.

When a namespace uses `vector` or `hybrid`, `memory_store` eagerly embeds the
record’s searchable text so subsequent semantic recall hits the cache. Callers
can also pass `options.embed: true` on `memory_store` to embed against an
otherwise lexical namespace, or `options.skip_embed: true` to suppress eager
embedding for one call.

Embeddings come from the host via the typed `memory.embed` capability:

| Request | Response |
|---|---|
| `{text: string, model_hint: string}` | `{vector: list<float>, model?: string, dim?: int}` |

Harn never bundles an embedding model. Hosts choose the model, handle rate
limiting, and decide cost accounting. For tests, register the capability via
`host_mock("memory", "embed", {result: {vector: [...], dim: N, model: "..."}})`.

Embeddings are cached on disk at
`.harn/memory/<namespace>/vectors/<sanitized_model_hint>/<sha256(text)>.json`.
The cache key is `(model_hint, content_hash)`, so swapping models invalidates
the cache without rewriting any records, and identical inputs always reuse the
same bytes.

## Replay

Memory is separate from transcript history. Runs that recall memory should
persist the recalled records in their run record before deterministic replay.

For vector and hybrid backends, the event log and the on-disk embedding cache
are the run record from memory’s perspective: as long as both survive into the
replay environment, recall returns the same ordered hits without re-invoking
the host. Embedding host calls are also recorded into the host-call mock log
so test fixtures can audit which texts were embedded under which model hint.
