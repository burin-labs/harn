- **Seven generated-artifact drift guards that never ran in CI now run on
  every PR.** `check-protocol-artifacts`, `check-session-bundle-schema`,
  `check-provider-catalog`, `check-provider-catalog-drift`,
  `check-docs-workflow-quickstart`, `check-receipt-structs`, and the new
  `check-tree-sitter-keywords` existed only in `make all`, so a contributor
  could edit `spec/openapi.yaml` or the `SessionBundle` DTO and ship stale
  bindings with green CI. They are now wired into the appropriate CI lanes.
  Wiring `check-docs-workflow-quickstart` surfaced a pre-existing stale pin:
  the workflow-authoring quickstart's `graph_digest` had drifted from the
  runtime; the docs page and check are re-pinned to the current value.
