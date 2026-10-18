- **Optional model cache storage.** `with_cache` preserves model results when a cache read or write fails.
  Unavailable reads do not produce false cache-miss receipts; a measured miss remains visible when a later write fails.
