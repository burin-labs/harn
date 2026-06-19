## Checkpoint & resume

Checkpoints enable resilient, resumable pipelines. State is persisted to the
resolved Harn state root (default `.harn/checkpoints/<pipeline>.json`) and survives crashes, restarts, and
migration to another machine.

### Core builtins

| Function | Description |
|---|---|
| `checkpoint(key, value)` | Save `value` at `key`; writes to disk immediately |
| `checkpoint_get(key)` | Retrieve saved value, or `nil` if absent |
| `checkpoint_exists(key)` | Return `true` if `key` is present (even if value is `nil`) |
| `checkpoint_delete(key)` | Remove a single key; no-op if absent |
| `checkpoint_clear()` | Remove all checkpoints for this pipeline |
| `checkpoint_list()` | Return sorted list of all checkpoint keys |

`checkpoint_exists` is preferable to `checkpoint_get(key) == nil` when `nil`
is a valid checkpoint value.

### std/checkpoint module

```harn
import { checkpoint_stage, checkpoint_stage_retry } from "std/checkpoint"
```

#### checkpoint_stage(name, fn) -> value

Runs `fn()` and caches the result under `name`. On subsequent calls with the
same name, returns the cached result without running `fn()` again. This is the
primary primitive for building resumable pipelines.

```harn
import { checkpoint_stage } from "std/checkpoint"

fn fetch_dataset(url) { url }
fn clean(data) { data }
fn run_model(cleaned) { cleaned }
fn upload(result) { log(result) }

pipeline process(task) {
  let url = "https://example.com/data.csv"
  let data    = checkpoint_stage("fetch",   fn() { fetch_dataset(url) })
  let cleaned = checkpoint_stage("clean",   fn() { clean(data) })
  let result  = checkpoint_stage("process", fn() { run_model(cleaned) })
  upload(result)
}
```

On first run all three stages execute. On a resumed run (pipeline restarted
after a crash), completed stages are skipped automatically.

#### checkpoint_stage_retry(name, max_retries, fn) -> value

Like `checkpoint_stage`, but retries `fn()` up to `max_retries` times on
failure before propagating the error. Once successful, the result is cached so
retries are never needed on resume.

```harn
import { checkpoint_stage_retry } from "std/checkpoint"

fn fetch_with_timeout(url) { url }

let url = "https://example.com/data.csv"
let data = checkpoint_stage_retry("fetch", 3, fn() { fetch_with_timeout(url) })
log(data)
```

### File location

Checkpoint files are stored at `.harn/checkpoints/<pipeline>.json` relative to
the project root (where `harn.toml` lives), or relative to the source file
directory if no project root is found. Files are plain JSON and can be copied
between machines to migrate pipeline state.

### std/agent_state module

```harn
import "std/agent_state"
```

Provides a durable, session-scoped text/blob store rooted at a
caller-supplied directory.

| Function | Notes |
|---|---|
| `agent_state_init(root, options?)` | Create or reopen a session root under `root/<session_id>/` |
| `agent_state_resume(root, session_id, options?)` | Reopen an existing session; errors when absent |
| `agent_state_write(handle, key, content)` | Atomic temp-write plus rename |
| `agent_state_read(handle, key)` | Returns `string` or `nil` |
| `agent_state_list(handle)` | Deterministic recursive key listing |
| `agent_state_delete(handle, key)` | Deletes a key |
| `agent_state_handoff(handle, summary)` | Writes a JSON handoff envelope to `__handoff.json` |

Keys must be relative paths inside the session root. Absolute paths and
parent-directory escapes are rejected.

### std/memory module

```harn
import "std/memory"
```

Provides a first-class durable memory log for observations that should be
available across later runs. The VM-native backend is append-only JSONL under
`.harn/memory/<namespace>/events.jsonl` by default. Callers may pass
`{root: "path"}` in options to place the memory root elsewhere. Namespace path
segments must be relative and cannot escape the memory root.

| Function | Notes |
|---|---|
| `memory_open(namespace, options?)` | Append a configuration event that selects the recall backend (`bm25`, `vector`, or `hybrid`) for this namespace |
| `memory_store(namespace, key, value, tags?, options?)` | Appends a memory observation and returns a `memory_record` dict |
| `memory_recall(namespace, query, k?, options?)` | Returns up to `k` active records ranked by the namespace backend (override per-call with `options.mode`) |
| `memory_summarize(namespace, window?, options?)` | Returns `{_type: "memory_summary", count, text, records}` for a recent or query-filtered slice |
| `memory_forget(namespace, predicate, options?)` | Appends a soft-delete event and returns `{forgotten, forgotten_ids}` |

Records contain `{id, namespace, key, value, text, tags, stored_at,
provenance}`. `memory_store` accepts `options.id`, `options.now`, and
`options.provenance`; `id` defaults to UUIDv7 and `stored_at` defaults to the
current UTC timestamp.

`memory_recall` defaults to deterministic local BM25: it tokenizes the key,
tags, text, and JSON value, then ranks active records with BM25 plus small
exact key/tag boosts. Pass `options.mode` (`lexical`, `semantic`, or `hybrid`)
to override the namespace default for one call.

`memory_open` configures the namespace backend without rewriting prior
records. The latest open event wins. Supported options:

- `backend` — `"bm25"` (default), `"vector"`, or `"hybrid"`.
- `embed_model_hint` — opaque model identifier passed to the host. Defaults
  to `"default"`.
- `embed_dim` — expected embedding dimensionality. Recall fails fast on a
  dimension mismatch.
- `bm25_weight` and `cosine_weight` — hybrid blend weights (defaults `0.5`
  each). Negative values are rejected.

When the namespace backend uses embeddings (or `memory_store` is called with
`options.embed: true`), Harn delegates to the typed host capability
`memory.embed`. Request shape: `{text: string, model_hint: string}`. Response
shape: `{vector: list<float>, model?: string, dim?: int}`. Harn never bundles
an embedding model; hosts choose it, and embeddings are cached on disk under
`.harn/memory/<namespace>/vectors/<sanitized_model_hint>/<sha256(text)>.json`
keyed by `(model_hint, content_hash)`. Tests can satisfy the capability with
`host_mock("memory", "embed", {result})`.

Determinism contract: a recall over the same `(namespace, query, mode,
embed_model_hint, top_k)` against the same event log and embedding cache
returns the same ordered hits, regardless of whether the host is still
attached.

`memory_summarize` is deterministic by default. `window` may be `nil`, an
integer limit, or a dict with `limit`, `query`, and `tag` / `tags`. The summary
text is an extractive bullet list capped to a bounded size; callers that need
LLM prose can pass `summary.records` to `llm_call`.

`memory_forget` never rewrites or removes prior observations. It appends a
tombstone event. Predicates may be a string (substring match against searchable
record text) or a dict using any combination of `id`, `key`, `tag` / `tags`,
and `query`; all provided dict predicates must match.

### std/agent/fact module

```harn
import { store_fact, recall_facts, invalidate_facts } from "std/agent/fact"
```

Provides typed `harn.fact.v1` assertions on top of `std/memory`. A fact contains
`kind`, `claim`, `evidence`, `confidence`, `provenance`, optional
`valid_until`, and `asserted_at`. Kinds normalize to `observation`, `claim`,
`hypothesis`, `decision`, or `constraint`; evidence kinds normalize to
`trace_ref`, `file_range`, `tool_output`, or `user_input`.

| Function | Notes |
|---|---|
| `fact(input, options?)` / `fact_validate(input)` | Normalize and validate a `harn.fact.v1` envelope; validation failures include `HARN-FACT-NNN` codes |
| `fact_namespace(scope?)` | Map `project`, `workspace`, or `user` to the default fact namespace, defaulting to `project/facts` |
| `fact_id(kind, claim, evidence?, provenance?)` | Build a stable `fact_...` id from normalized assertion fields |
| `fact_key(fact)` | Return the reserved `fact:<kind>:<id>` memory key |
| `fact_tags(fact, tags?)` | Return `fact`, `fact:<kind>`, `schema:harn.fact.v1`, generic and kind-scoped evidence tags, and caller tags |
| `store_fact(input, options?)` | Store the fact as `MemoryRecord.value`, using the fact id as the memory record id |
| `recall_facts(query, kind?, min_confidence?, scope?)` | Recall normalized facts and filter by kind/confidence; `scope` may be a scope string or an options dict with normal memory options |
| `invalidate_facts(predicate, scope?)` | Append memory tombstones; predicates accept exact `fact_...` ids or dicts with `id`, `key`, `kind`, `claim`, `query`, `tag`, `tags`, `evidence_ref`, or `evidence` |

### std/agent/probe module

```harn
import { probe, probe_eval, probe_typecheck } from "std/agent/probe"
```

Provides a probe-first verification primitive: instead of asserting a
claim about runtime behavior, run a small snippet, capture the outcome
deterministically, and auto-record it as a `harn.fact.v1` Observation
backed by `std/agent/fact`. Probes return a `harn.probe.v1` envelope
with `kind`, `outcome` (`pass` / `fail` / `unknown`), `observed` summary,
`evidence` (trace id, snippet, command, stdout/stderr/exit_code,
duration_ms), `fact_id` of the auto-stored observation, and the
round-tripped `expected` and `asserted_at`. MVP supports `eval` and
`typecheck`; `test` and `inspect` are reserved and currently return an
`unknown`-outcome envelope so callers can wire them in deterministically.

| Function | Notes |
|---|---|
| `probe(kind, body, options?)` | Run a snippet (`eval` or `typecheck` today; `test`/`inspect` reserved) and capture the outcome. `options.expected` enables a hard match (string for stdout, int for typecheck error count); without it, outcome derives from exit code or error count. |
| `probe_eval(body, options?)` | Convenience for `probe("eval", ...)`. Runs `body` as a shell command by default; `options.lang = "harn"` writes the body to a temp file and runs `harn run <path>`. |
| `probe_typecheck(body, options?)` | Convenience for `probe("typecheck", ...)`. Writes the fragment to a temp file and runs `harn check <path> --json`, parsing the summary for error and warning counts. |

Unless `options.store_fact = false`, every probe writes an Observation
with `confidence = 0.9` for observed `pass`/`fail` and `0.4` for
`unknown`, `provenance.source = "probe"`, and
`provenance.probe_kind = "<kind>"`. Pass `options.scope`,
`options.namespace`, or `options.root` to override the fact-store
location, and `options.harn_binary` to point at a specific Harn CLI
build for `typecheck` (and `harn`-lang `eval`) probes.

Validation failures throw with `HARN-PROBE-NNN` diagnostic codes:
`001` (options), `002` (kind), `003` (lang), `004` (body).

### std/postgres module

```harn
import "std/postgres"
```

Provides VM-native Postgres helpers for durable tenant state, event logs,
receipts, claims, and audit records.

| Function | Notes |
|---|---|
| `pg_pool(source, options?)` | Open a pooled Postgres connection from a URL, `env:NAME`, `secret:namespace/name`, or source dict |
| `pg_connect(source, options?)` | Open a single-connection Postgres pool |
| `pg_query(handle, sql, params?)` | Run a parameterized query and return rows as dictionaries |
| `pg_query_one(handle, sql, params?)` | Return the first row, or `nil` when no rows match |
| `pg_execute(handle, sql, params?)` | Execute a statement and return `{rows_affected}` |
| `pg_transaction(pool, callback, options?)` | Pass a scoped transaction handle to a closure, commit on success, rollback on thrown error |
| `pg_close(pool)` | Close a pool handle |
| `pg_stmt_cache_clear(pool)` | Clear prepared-statement caches on idle primary and replica connections |
| `pg_mock_pool(fixtures)` | Create an in-process fixture-backed Postgres handle for tests |
| `pg_mock_calls(mock)` | Inspect recorded mock SQL calls |

Dynamic values must be passed through the `params` list rather than string
interpolation. Compound Harn values bind as JSON. Row decoding covers null,
boolean, integer and float types, text, `uuid`, `json`/`jsonb`, `bytea`, date,
time, timestamp, and timestamptz. Transaction options may include `settings`,
applied with transaction-local `set_config(name, value, true)` for RLS
policies. Schema migrations are host-owned; Harn does not maintain a migration
ledger.

