- **LLM provider rate/concurrency governor — Layer 0 detection + Layer 1 adaptive
  governor, behind the default-off `llm.rate_governor` flag (`HARN_LLM_RATE_GOVERNOR=1`).**
  When armed, Harn now governs its own concurrency/rate per `(provider, org_key)`
  against provider throttling instead of retrying blindly into an org-wide wall. Detection
  classifies each provider outcome into a structured `provider_throttle` transcript
  record (HTTP 429, 529/503 overload, Anthropic overloaded/rate-limit body, and the
  empty-completion-under-load heuristic). The governor runs an AIMD concurrency
  limiter (additive-increase on sustained success, halve-to-floor on a throttle
  signal), optional RPM/TPM token buckets, and a circuit breaker
  (CLOSED → OPEN with exponential backoff + full jitter honoring `Retry-After`
  → HALF-OPEN single probe → CLOSED) so retries wait behind the governor. Limits
  live in the catalog as `[provider_limits.<provider>]` rows (Anthropic seeded;
  every provider generic), never at call sites. New `harn provider limits`
  reports resolved limits and live governor state deterministically, and a
  `governor_state` record plus a `circuit_is_open` query seam expose whether a
  run was infra-throttled rather than a capability failure. Byte-identical
  behavior when the flag is off. Layer 2 (shared-local Harn leases) and
  Layer 3 (Harn Cloud quota authority) are follow-ups; the `(provider, org_key)`
  key and serializable governor state leave clean seams for both.
