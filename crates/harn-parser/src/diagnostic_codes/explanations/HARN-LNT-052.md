# HARN-LNT-052 — ambient clock builtin replaced by `harness.clock.*`

## What it means

The lint fires on any call to `now_ms`, `monotonic_ms`, `sleep_ms`,
`timestamp`, or `elapsed`. These were ambient clock-capability builtins in
the pre-`Harness` runtime. Time access now routes through the
`harness.clock.*` sub-handle so capability requirements appear in the type
system instead of being hidden in the stdlib surface.

This is a lint, not a hard error. The legacy builtins still compile while
the migration is in flight, but every new call site should use the
`harness.clock.*` method that matches it (`now_ms` → `harness.clock.now_ms`,
`sleep_ms` → `harness.clock.sleep_ms`, etc.).

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. By default the
  fixer rewrites ambient clock calls to the VM-level `harness` binding with
  `bindings/use-enclosing-harness-global`, preserving helper signatures.
- If you explicitly want source-level parameter threading, run
  `harn fix --apply --safety surface-changing --harness-threading thread-params`.
  `harn fix --plan --json` reports which signatures would change and whether
  cross-module callers must be updated.
