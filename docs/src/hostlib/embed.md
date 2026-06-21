# Text similarity / embeddings (hostlib)

The `embed` capability is a cross-platform, fully-offline core for
cosine/semantic similarity. It is the single source of truth two consumers
share: Burin's push-context Tier-2 (auto-injecting skills/canon/memory/
few-shot above a similarity threshold) and Burin's `SymbolRelevance` symbol
ranking (today split between macOS-only `NLEmbedding` and a Linux Jaccard
fallback). It is registered automatically by
`harn_hostlib::install_default` and exposes four builtins:

| Builtin                       | Returns                                          |
|-------------------------------|--------------------------------------------------|
| `hostlib_embed_similarity`    | `{similarity, relatedness}`                      |
| `hostlib_embed_top_k`         | `{results: [{index, text, score, relatedness}]}` |
| `hostlib_embed_vector`        | `{dim, vector}`                                  |
| `hostlib_embed_info`          | `{backend, dim}`                                 |

`similarity` is the raw cosine in `[-1, 1]`; `relatedness` is the same value
clamped to `[0, 1]` so each consumer picks the shape it wants without
re-deriving it. `top_k` ranks a corpus of strings against a query, highest
first, with deterministic tie-breaking (ascending index) for reproducible
evals, and an optional `min_score` threshold.

## Backends

The capability owns one embedding backend behind an `Arc`, shared across
every VM/thread (mirroring `code_index`). Embedding is `text -> fixed-dim
f32 vector`; `embed::similarity` then provides backend-agnostic cosine and
top-k. Two pure-Rust backends ship in-tree:

| Backend            | `name`             | Asset      | Latency      | Quality                    |
|--------------------|--------------------|------------|--------------|----------------------------|
| Lexical (default)  | `lexical-hash`     | none       | ~microsecond | lexical / char-ngram       |
| Static (Model2Vec) | `static-model2vec` | ~MB on disk| ~microsecond | ~92% of MiniLM-class (asset)|

**Lexical (default).** A hashed bag-of-features — word tokens
(camelCase/snake_case-split, identifier-aware) plus char trigrams — projected
into a fixed 256-dim space via the signed hashing trick, then L2-normalized.
No model, no asset, no network; deterministic across macOS/Linux/Windows
(it uses a stable FNV-1a hash, not the stdlib's stability-free hasher). This
is the graceful-degradation floor every other backend falls back to.

**Static (Model2Vec / "potion"-style).** Precomputed per-token vectors are
loaded from a vendored asset, then `tokenize -> lookup -> mean -> normalize`
with **no neural-network inference** — that is what makes static embeddings
microsecond-fast and tiny. When a query contains no known tokens it falls
back to the lexical projection so a novel identifier is still comparable.

A higher-accuracy on-device transformer tier (candle / ONNX, loading
`bge-small`/MiniLM `.safetensors`) can be added later behind a Cargo feature
**without changing this surface or either consumer**: implement the
`Embedder` trait, resolve the asset the same way, and register it as the
active backend when the feature and setting are on.

## Asset resolution (sandbox- and settings-aware)

`EmbedCapability::resolve(override_dir, data_dir, model)` selects the static
backend when an asset is resolvable, otherwise the lexical floor. Resolution
order:

1. explicit `override_dir` (from a Harn setting / host-call parameter),
2. `<data_dir>/embeddings/<model>/static-embeddings.json` (conventional
   vendored location).

The asset is `static-embeddings.json`:

```json
{ "dim": 256, "vectors": { "rate": [/* 256 floats */], "limit": [/* ... */] } }
```

A missing, unreadable, malformed, or empty asset **never panics and never
blocks** — it just selects the lexical floor. The resolver never touches the
network and never reads outside the provided roots, so it is safe to call
inside a sandbox.

## Performance

See `cargo run -p harn-hostlib --release --example embed_latency_ladder` for
a latency ladder (single-embed and top-k p50/p99 across corpus sizes
10/100/1k/10k, cold-start, and approximate peak memory). Both default
backends embed in microseconds and the cosine math is a tight `f32` loop, so
top-k over 10k items stays well under a millisecond on a modern core.
