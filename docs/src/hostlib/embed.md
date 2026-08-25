# Text similarity / embeddings

The `embed` capability is a cross-platform, fully-offline core for cosine
similarity. Hosts register one implementation and scripts reach it only
through the `hostlib_embed_*` builtins / `harness.embed` surface:

| Method                        | Returns                                                            |
|-------------------------------|--------------------------------------------------------------------|
| `harness.embed.similarity`    | `{similarity, relatedness}`                                        |
| `harness.embed.top_k`         | `{backend, is_semantic, results: [{index, text, score, relatedness}]}` |
| `harness.embed.vector`        | `{dim, vector}`                                                    |
| `harness.embed.info`          | `{backend, dim, is_semantic}`                                       |

`similarity` is the raw cosine in `[-1, 1]`; `relatedness` is the same value
clamped to `[0, 1]`. `top_k` ranks a corpus of strings against a query,
highest first, with deterministic tie-breaking (ascending index). Both
`info` and `top_k` name the active backend and whether it has earned a
semantic ranking claim. Shipped backends report `is_semantic: false`.

## Backends

The capability owns one embedding backend behind an `Arc`. Embedding is
`text -> fixed-dim f32 vector`; cosine math is backend-agnostic.

| Backend            | `name`             | Asset                         | `is_semantic` |
|--------------------|--------------------|-------------------------------|---------------|
| Lexical (default)  | `lexical-hash`     | none                          | `false`       |
| Static (Model2Vec) | `static-model2vec` | operator-supplied JSON table  | `false`       |
| Local ONNX encoder | `onnx-minilm`      | opt-in catalog install        | `false`       |

**Lexical (default).** A hashed bag-of-features projected into a fixed
256-dim space, then L2-normalized. No model, no asset, no network.

**Static.** Precomputed per-token vectors loaded from an operator-supplied
`static-embeddings.json`. Harn does not ship a table. A missing or malformed
file selects the lexical floor; the backend still reports `is_semantic:
false` until a ranking audit earns the claim.

**Local ONNX encoder.** Opt-in. `harn guard install minilm-l6-v2
--accept-license` fetches a cataloged Apache-2.0 MiniLM ONNX package into
the guard store. CLI builds with `--features guard-neural` load it when
present. A missing, corrupt, or unloadable install never fails a query —
the lexical floor stays in place. This path also reports `is_semantic:
false`.

## Asset resolution

`EmbedCapability::from_env()` / `resolve(override_dir, data_dir, model)`
selects the static backend when an asset is resolvable, otherwise lexical.
Resolution order:

1. `HARN_EMBED_ASSET_DIR` (explicit override directory),
2. `<state>/embeddings/<model>/` where `<model>` comes from
   `HARN_EMBED_MODEL` (default `default`).

The static asset is `static-embeddings.json`:

```json
{ "dim": 256, "vectors": { "rate": [/* 256 floats */], "limit": [/* ... */] } }
```

A missing, unreadable, malformed, or empty asset **never panics and never
blocks** — it selects the lexical floor. The resolver never touches the
network.

## Provider catalog

Hosted embeddings APIs are declared on provider rows (`embeddings_endpoint`
plus an `embeddings` feature) and on model rows (`embedding_dim`,
`embedding_max_tokens`). Those catalog rows are generated from
`crates/harn-vm/src/llm/catalog_sources/`; do not hand-edit
`spec/provider-catalog/` or `crates/harn-vm/src/llm/providers.toml`.
