- **`harn test` now supports duration-aware user-test sharding.** User test
  suites can pass `--shard-index` and `--shard-total` (or the matching
  `HARN_TEST_SHARD_INDEX` / `HARN_TEST_SHARD_TOTAL` environment variables) to
  split CI matrix work after discovery and balance shards using the existing
  timing cache.
