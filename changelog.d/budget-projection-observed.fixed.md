- **Pre-call budget projection now prices the next call from the session's own calls.**
  A cached session with short answers could be stopped by `total_budget_usd`
  at roughly half its cap: the pre-call estimate priced every projected input
  token at the uncached rate and assumed the whole output budget was spent, so
  one small call was projected ~18x its real cost. From the second call of a
  session on, the estimate uses the observed cache-hit ratio and the observed
  mean output tokens per call (clamped to the output budget); the first call
  keeps the conservative worst case. Token ceilings and rate-limit
  reservations still use the full output budget.
- **`budget_exceeded` errors and `budget_exhausted` events say which happened.**
  New `projection_basis` (`observed` / `worst_case`), `headroom_usd`, and
  `costed_output_tokens` fields, plus a message that names spend against the
  cap, so "stopped by an estimate at $0.18 of $0.33" is distinguishable from
  "spent the cap". `sessionCostUsd`, `projectionBasis`, and `headroomUsd`
  project through ACP.
