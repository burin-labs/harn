- **Test isolation: the persona portal-status test takes the harn-state
  lock.** `api_personas_exposes_runtime_status` mutated event-log state
  governed by `HARN_EVENT_LOG_*` env vars without holding `lock_harn_state`,
  so a concurrent lock-holder's environment could route its pause event into
  the wrong log and the test would flakily read the persona as idle.
