- **LLM retry/throttle layer is more robust under concurrency and network loss.**
  Three deterministic, unit-tested hardenings to the outbound-LLM retry path:
  - **Equal-jitter backoff.** Transient-error backoff was a fixed exponential
    (`250/500/1000/2000/4000ms`) with zero jitter, so concurrent same-key
    callers (e.g. `eval --concurrency K` alongside a live session) retried in
    lockstep and re-stampeded the provider. Backoff now uses AWS "equal jitter"
    (`wait = ceil/2 + rand(0, ceil/2)`, `ceil = 250 * 2^min(attempt, 4)`), which
    desynchronizes retries while avoiding the near-zero waits of full jitter. A
    small additive jitter is also layered on top of an honored provider
    `Retry-After` so identical `Retry-After` values do not resume in unison.
  - **Typed non-streaming send errors.** Streaming `req.send()` failures were
    already classified by reqwest error kind, but non-streaming send failures
    became a bare `"{provider} API error: {e}"` string that the retry layer
    had to re-classify by substring. Both paths now share one reqwest-kind
    classifier that tags timeouts/connection failures as typed
    `ErrorCategory::Timeout` / `TransientNetwork` at the source.
  - **Network-only circuit breaker.** A per-route, per-process breaker now opens
    after sustained `NetworkError`/`Timeout` failures (laptop disconnect, DNS
    failure, dropped link), fails fast for a short window, then admits a single
    half-open probe and closes on success. It deliberately does **not** react to
    429 (handled by the existing rate-limiter cooldown + `Retry-After`) or 5xx,
    so it only stops the retry budget from burning against a truly dead link.
