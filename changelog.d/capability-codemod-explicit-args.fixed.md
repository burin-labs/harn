- **Capability migrations now preserve compact orchestration and call shapes.**
  `harn fix --capability-migrations-only` keeps three-or-more-capability
  orchestration on root `Harness`, recognizes local and imported named
  capability bundles, rewrites newly split accesses to their explicit
  bindings, and replaces existing imported carrier arguments in place instead
  of shifting ordinary arguments.
