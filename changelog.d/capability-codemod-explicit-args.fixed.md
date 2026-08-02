- **Capability migrations now preserve compact orchestration and call shapes.**
  `harn fix --capability-migrations-only` keeps three-or-more-capability
  orchestration on root `Harness`, recognizes local and imported named
  capability bundles, rewrites newly split accesses to their explicit
  bindings, and replaces existing imported carrier arguments in place instead
  of shifting ordinary arguments. A root requirement now widens a narrow
  carrier in place, stale ambient receiver names are rebound to the explicit
  carrier, and typed arity diagnostics let zero-argument imported helpers gain
  their omitted capability. Multiline declarations and calls retain clean line
  endings when the migration inserts a leading capability.
