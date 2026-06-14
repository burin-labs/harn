- **`harness.auth` — a read-only authenticated-principal handle for `.harn`
  routes.** A `harn-serve` dispatch now threads the principal it
  authenticated at admission — subject, scheme, granted scopes, and an
  optional embedder-assigned principal `kind` — to the `.harn` callee as the
  ambient `harness.auth` sub-handle, alongside the existing `harness.tenant`.
  Routes can read identity and compose their own authorization without a
  host-side dispatch guard: `harness.auth.is_authenticated()`,
  `harness.auth.subject()` / `try_subject()`, `harness.auth.scheme()` /
  `try_scheme()`, `harness.auth.kind()`, `harness.auth.scopes()`, and
  `harness.auth.has_scope(scope)`. `subject()`/`scheme()` raise a typed
  `Auth` error when no principal is bound (mirroring `harness.tenant.id()`);
  the `try_*`/`kind` getters return `nil` and `scopes()`/`has_scope()`/
  `is_authenticated()` degrade to empty/false so an unauthenticated route can
  branch without try/catch. The handle is identity-only: it carries no
  credentials or secrets, never the tenant (that stays the single-sourced
  `harness.tenant` ambient), and never the opaque embedder auth context (that
  stays the host-call-bridge channel). The synthetic anonymous principal
  harn-serve admits under allow-all binds nothing, so `is_authenticated()` is
  `false` when no credential authenticated the request. Foundation for
  Harn-side route auth policies (issue #3323); unblocks harn-cloud's adoption
  of declarative route policies in place of duplicated Rust dispatch guards.
