- **Ollama truncation visibility & cache mislabeling.** The `/api/chat` NDJSON
  done-frame parser now captures Ollama's `done_reason` into `stop_reason`, so
  length-truncation is visible on the most-used local chat path (it was
  hard-coded to `None`). A `done_reason == "length"` cut-off no longer surfaces
  as the retryable `[ollama_empty_content_parser_bug]` error — it returns
  cleanly with `stop_reason: "length"`, a non-retryable signal, so the retry
  loop no longer spins re-truncating a deterministic token cap. Native Ollama
  responses (`/api/chat`, `/api/generate`, completion) now report cache as
  `cache_visibility: "unsupported"` with a null `cache_hit_ratio` instead of a
  fabricated `0.0` ratio that scored a local model as a 100% cache miss.
