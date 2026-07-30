# HARN-LNT-055 — ambient env builtin replaced by `harness.env.*`

## What it means

The lint fires on calls to the ambient `env` and `env_or` builtins.
Environment access now routes through the `harness.env.*` sub-handle so
capability requirements appear in the type system instead of being
hidden in the stdlib surface.

The legacy effectful globals are removed. This lint supplies an actionable
migration repair before the checker reports the removed symbol.

## How to fix

- Run `harn fix --apply --safety surface-changing` over the file. Calls inside
  an existing Harness boundary are rewritten in place; otherwise the fixer
  threads an explicit Harness parameter through local callers.
- Run lint again. `capability-attenuation` suggests replacing an unnecessarily
  broad helper parameter with `HarnessEnv`.
