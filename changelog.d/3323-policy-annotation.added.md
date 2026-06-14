- **`@policy(kinds: "...")` — a declarative route auth policy for
  `harn serve site`.** A routed `pub fn` can now declare the principal
  kinds permitted to invoke it, composing with `@scopes` rather than
  replacing it: `@policy(kinds: "operator platform_admin")` admits a
  request only when the embedder-resolved principal `kind` (see
  `harness.auth.kind()`) is in the allow-set. Enforcement runs at site
  admission immediately after the scope check and **fails closed** — a
  principal the embedder did not classify can never satisfy a non-empty
  allow-set. Denials render the tenant-safe `forbidden_principal_kind`
  403 envelope, which names the route's allowed kinds (route
  configuration) but never echoes the caller's own kind. The parsed policy
  is carried on the route's export entry (`ExportedFunction.policy`) so
  audit tooling can see which routes declare a principal-kind guard. A
  malformed argument (unknown key, positional, or non-string value) is
  dropped with a `HARN-SRV-017` diagnostic and a typechecker warning,
  leaving any host-side defense-in-depth check in place. Together with the
  `harness.auth` handle this is the declarative half of the Harn-side route
  auth policy (issue #3323); the imperative `require_policy(...)` helper for
  method-specific / resource-match cases is a follow-up.
