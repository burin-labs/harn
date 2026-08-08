Unified LLM token, cost, cache, and serving-tier accounting behind one Rust
ledger. VM responses, provider-response events, traces, metrics, and provider
probes now project the same normalized values; LLM trace events use the
canonical `cache_read_tokens` and `cache_write_tokens` fields instead of the
older `cache_tokens` mirror.
