Enable SQLite WAL mode (with `busy_timeout` + `synchronous=NORMAL`) on the LLM read cache
(`llm.sqlite`) and the durable rate limiter (`llm-rate-limits.sqlite`) so multiple concurrent
Harn sessions on one machine no longer hit `SQLITE_BUSY` (silently dropped usage rows / blocked
LLM calls). Matches the WAL pattern already used by `events.sqlite`.
