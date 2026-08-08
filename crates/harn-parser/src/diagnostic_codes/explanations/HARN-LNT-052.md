# HARN-LNT-052 — ambient clock builtin replaced by `harness.clock.*`

## What it means

The lint fires on any call to `now_ms`, `monotonic_ms`, `sleep_ms`,
`timestamp`, or `elapsed`. These were ambient clock-capability builtins in
the pre-`Harness` runtime. Time access now routes through the
`harness.clock.*` sub-handle so capability requirements appear in the type
system instead of being hidden in the stdlib surface.

The legacy effectful globals are removed. This lint supplies an actionable
migration repair before the checker reports the removed symbol.

## How to fix

- Run `harn fix --apply --safety surface-changing` over the file. Calls inside
  an existing Harness boundary are rewritten in place; otherwise the fixer
  threads an explicit Harness parameter through local callers.
- Run lint again. `capability-attenuation` suggests replacing an unnecessarily
  broad helper parameter with the narrow nominal handle it actually uses.
