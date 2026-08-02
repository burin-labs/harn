- **Capability migrations now preserve split receivers and imported-call arity.**
  `harn fix --capability-migrations-only` rewrites newly split capability
  accesses to their explicit bindings and replaces existing imported carrier
  arguments in place instead of shifting ordinary arguments.
