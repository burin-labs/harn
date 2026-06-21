Added an `embed` hostlib capability: a cross-platform, fully-offline
text-similarity / embedding core exposing `hostlib_embed_similarity`,
`hostlib_embed_top_k`, `hostlib_embed_vector`, and `hostlib_embed_info`.
The default backend is an always-available, zero-asset lexical hashing
embedder (deterministic across macOS/Linux/Windows, microsecond latency); a
Model2Vec/"potion"-style static token-pooled backend is selected
automatically when a vendored asset is resolvable (sandbox/settings-aware,
no network), degrading cleanly to lexical when absent. A higher-accuracy
candle/ONNX transformer tier can slot in behind a future Cargo feature
without changing the surface.
