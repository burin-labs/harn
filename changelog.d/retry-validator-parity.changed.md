- **`@retry(...)` and `@job(retry: {...})` now share one validator.** The
  standalone job modifier and its compact dict alias are documented as
  equivalent, so their recognized keys and backoff strategies
  (`svix`/`linear`/`exponential`) are now a single source of truth — the two
  surfaces can no longer drift apart in what they accept or reject.
