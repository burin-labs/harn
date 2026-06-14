- **`std/harness/policy.require_policy` — imperative route auth-policy guard.**
  The second half of the route-policy toolkit (the declarative
  `@policy(kinds: ...)` annotation is the first): a `.harn` handler can call
  `require_policy({kinds: [...], scopes: [...]})` to enforce a principal-kind
  / scope policy that depends on runtime data the annotation cannot see (a
  path or body field, resource ownership). It composes the ambient
  `harness.auth` principal and returns `nil` when the policy is satisfied, or
  a ready-to-return tenant-safe HTTP 403 envelope (`http_error`) when it is
  not — `let denial = require_policy({...}); if denial != nil { return denial }`.
  Denials name the route's requirement (`allowed_kinds`, the missing scope)
  but never echo the caller's own kind, matching the `@policy` denial, and
  fail closed: an unauthenticated or unclassified principal never satisfies a
  non-empty `kinds` allow-set.
