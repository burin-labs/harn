- **`orchestrator_worker_claim_expiry_requeue` conformance test
  de-flaked under load.** The original `sleep(120ms)` after the failing
  drain assumed the first subprocess's 25ms heartbeat (TTL/2 with
  TTL=50ms) would have stopped by the time the second drain spawned.
  Under heavy parallel cargo load on the same host the second `harn`
  process could cold-start and call `claim_next` while the last
  heartbeat was still alive, see zero ready jobs, and trip the
  reclaim assertion. Replaced with a poll-with-budget loop that
  re-runs `drain` (idempotent — a no-op drain returns
  `drained:0` without consuming state) at 50ms intervals up to 5s
  until `drained == 1 && acked == 1 && deferred == 0`. Same code
  shape as `wait_for_readyz` in
  `orchestrator_recover_stranded_envelopes.harn`.
