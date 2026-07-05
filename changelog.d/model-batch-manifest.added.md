- **Model batch manifests.** `harn models batch manifest` now turns JSONL
  request ledgers into durable provider-neutral batch manifests with grouped
  requests, stable custom ids, row hashes, and catalog-backed provider/model
  metadata for offline eval, judge, corpus-refresh, and distillation jobs.
