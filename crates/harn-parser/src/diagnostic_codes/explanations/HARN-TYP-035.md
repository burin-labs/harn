# HARN-TYP-035: Predicate model operation is unavailable

Predicate evaluation requires an explicitly declared `decision` operation on
the selected catalog route. The current `structured_llm` backend also requires
`text_generation`. An embedding route, unknown model, or decision-only native
route cannot inherit a generic chat capability from its provider.

Declare a compile-time constant policy with a provider and model whose catalog
operations satisfy the backend. A policy supplied only at runtime cannot prove
this check-time obligation. Check the named model and missing operation in the
diagnostic; do not add an operation merely to silence the checker without
evidence that the route supports it.

This check makes no provider request and establishes neither credential
availability nor model quality. Runtime admission still owns provider options,
authority, resource reservations, and the evaluation outcome.
