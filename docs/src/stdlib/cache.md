# Cache stdlib

`std/cache` is the content-addressed cache substrate that lets every "I
already computed this" decision share one governed implementation: LLM
calls, repo scans, evidence-gathering for hypotheses, fixture loads for
crystallization shadow runs, deterministic queries inside context packs.

Three backends share the same envelope shape:

| Constructor | Backing | Survives `harn run`? | Best for |
|---|---|---|---|
| `mem_cache(opts?)` | thread-local LRU | no | short-lived eval/shadow runs |
| `fs_cache(path, opts?)` | one JSON file per key | yes | small-team caches checked into a repo |
| `sqlite_cache(path, opts?)` | one sqlite file | yes | high-traffic shared caches |

All three accept the same options: `namespace`, `ttl` (e.g. `"10m"`) or
`ttl_seconds`, and `max_entries` (LRU bound).

## Primitives

```harn
import { cache_clear, cache_get, cache_put, cache_stats, mem_cache } from "std/cache"

pipeline default() {
  let store = mem_cache({namespace: "evals", ttl: "5m"})
  cache_put("k", {answer: 42}, {store: store})
  let hit = cache_get("k", {store: store})
  // -> {hit: true, value: {answer: 42}, backend: "mem", namespace: "evals"}
  println(to_string(hit.hit))
  cache_clear({store: store})
  println(json_stringify(cache_stats({store: store})))
}
```

`cache_stats(options?)` returns `{hits, misses, lookups, hit_rate}` for
the namespace identified by `options`. The counters live in-process and
reset whenever the process restarts or `cache_clear` / `cache_stats_reset`
is called.

## `with_cache` — composable memoization

`with_cache(key, compute, options?)` is the recommended call site. The
wrapper looks up `key` in `options.store`, returns the cached value on a
hit, or runs `compute()` and stores the result on a miss.

```harn
import { mem_cache, with_cache } from "std/cache"

fn deep_scan_repo() -> dict {
  return {files: 42}
}

pipeline default() {
  let store = mem_cache({namespace: "scan-results", ttl: "1h"})
  let scan = with_cache("repo:abc123", fn() { return deep_scan_repo() }, {store: store})
  println(json_stringify(scan))
}
```

`with_cache_envelope` returns `{value, hit, key, metrics}` instead of
just the value, so callers can branch on cache state and surface the
receipts. The metrics dict carries `{compute_ms}` on misses and (by
default) `{model_calls_avoided: 1}` on hits — pass `options.estimate`
to enrich the hit receipts with `tokens_saved` and `latency_saved_ms`.

When `options.session_id` is set, `with_cache` emits `cache.hit` and
`cache.miss` events on the agent event tape. The tape is the
record/replay surface for crystallization shadow runs and persona value
ledgers — cached calls show up there as durable receipts instead of
ghosting through unobserved.

## LLM caller-wrapper form

`std/llm/handlers` re-exports `with_cache` as a composable middleware
that sits inline in the LLM caller stack:

```harn,ignore
import { sqlite_cache } from "std/cache"
import { compose, default_llm_caller, with_cache, with_retry } from "std/llm/handlers"

let caller = compose([
  with_retry({max_attempts: 3}),
  with_cache({store: sqlite_cache(state_path("llm.sqlite"))}),
])(default_llm_caller())
```

The wrapper keys on the canonical `llm_cache_key(prompt, system, opts)`
sha256 so identical `(prompt, system, opts)` triples are byte-for-byte
deterministic across runs. Calls that carry `opts.tools` skip the cache
by default — tool-using LLM calls are usually side-effectful.

On a hit, the wrapper records `model_calls_avoided: 1` plus
`tokens_saved` (pulled from the cached envelope's `usage`) and
`latency_saved_ms` (pulled from `latency_ms`) on the agent event tape,
which is what the persona value ledger and crystallization receipts
read.

## Adoption recipes

### Persona caller cache

A persona that asks the same question against the same context many
times — e.g. classifying inbound issues — wins big from caching:

```harn,ignore
import { sqlite_cache } from "std/cache"
import { compose, default_llm_caller, with_cache, with_retry } from "std/llm/handlers"

let caller = compose([
  with_retry({max_attempts: 2}),
  with_cache({store: sqlite_cache(state_path("personas/triage.sqlite"), {ttl: "24h"})}),
])(default_llm_caller())
```

### Deterministic context-pack query

Context packs include repeatable read-only queries (e.g. "list the
files under `crates/foo` matching pattern X"). Wrapping the query in
`with_cache` keeps the pack stable across reruns:

```harn,ignore
import { mem_cache, with_cache } from "std/cache"

let store = mem_cache({namespace: "context-pack-" + run_id})
let files = with_cache("files:" + repo_sha + ":" + pattern, fn() {
  return list_files(pattern)
}, {store: store})
```

### Crystallization shadow-run fixture

Shadow runs replay recorded traces against a candidate workflow.
Wrapping the fixture loader in `with_cache` against a fixture file
(`fs_cache`) guarantees bit-exact replay across runs:

```harn,ignore
import { fs_cache, with_cache } from "std/cache"

let fixtures = fs_cache(repo_path("conformance/fixtures/triage"))
let fixture = with_cache("trace:" + trace_id, fn() {
  return read_jsonl(repo_path("conformance/fixtures/triage/" + trace_id + ".jsonl"))
}, {store: fixtures, session_id: session_id, estimate: {model_calls_avoided: 1}})
```

Because the session_id is set, the shadow run's tape captures the
`cache.hit` events for every replay — the crystallization receipts and
persona value ledger read them back to show "model calls avoided" in
the demo from the [Moat Addendum: Workflow Crystallization](https://www.notion.so/34a7d224c63281fcbc13cf390981b01b).

## Replay determinism

Cached calls are deterministic. The cache key is a sha256 of the
canonical-JSON-sorted identity for the call, and the cached value is
the verbatim envelope from the original miss. Replaying a recorded
tape that contains `cache.hit` events does not touch the underlying
model or filesystem — the cached value is returned byte-identical.
TTL expiry honors the unified clock (`mock_time` / `advance_time`), so
testbench fixtures can reproduce expiry windows without wall-clock
flakiness.
