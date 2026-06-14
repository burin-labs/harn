- **`llm_cost` returns an exact `decimal` instead of a binary `float`.** The
  per-call cost is money, so it is now computed and returned as a `decimal`:
  summing many calls no longer drifts, and the value compares exactly. Each
  catalog rate is recovered to its *authored* decimal value (the short literal
  in `providers.toml`, e.g. `0.15`) via shortest-round-trip recovery, so the
  result is genuinely exact rather than `float`-rounding laundered into
  false precision. `llm_format_usd` now accepts a `decimal` amount (alongside
  `float`/`int`), so `llm_format_usd(llm_cost(...))` keeps working. This is a
  breaking type change for scripts that compared `llm_cost(...)` against a
  `float` literal — `decimal` is a clean island and never compares
  equal/ordered with `float`, so compare against `decimal("…")` instead. The
  `@budget` enforcement accumulator and `llm_session_cost`/`llm_pricing`/
  `llm_compare_costs` continue to report `float` for now; migrating that
  family to `decimal` is tracked separately.
