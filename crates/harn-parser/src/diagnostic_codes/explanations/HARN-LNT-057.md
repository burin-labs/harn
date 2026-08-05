# HARN-LNT-057 — ambient net builtin replaced by `harness.net.*`

## What it means

The lint recognizes removed ambient network calls and supplies a migration
repair. Requests, downloads, streams, sessions, SSE, WebSockets, and servers
all route through the `HarnessNet` interface so capability requirements appear
in the type system instead of being hidden in a global surface. Pure response
constructors and event encoders remain ordinary globals.

The legacy effectful globals are not a compatibility surface: ordinary source
must use the corresponding `harness.net.*` method. The lint exists so old
source gets an actionable repair before the checker reports the removed
symbol.

## How to fix

- Run `harn fix --apply --safety surface-changing` over the file. Calls inside
  an existing Harness boundary are rewritten in place; otherwise the fixer
  threads an explicit Harness parameter through local callers.
- Run lint again. `capability-attenuation` suggests replacing an unnecessarily
  broad helper parameter with `HarnessNet`.
