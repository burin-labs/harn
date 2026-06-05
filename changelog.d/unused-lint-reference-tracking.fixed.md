- **Unused-symbol linting now counts references from callable defaults and
  type-only positions.** Imports, parameters, locals, and types referenced from
  default parameter expressions, keyed mutex expressions, binding annotations,
  pipeline return annotations, closure parameter annotations, typed catch
  clauses, explicit generic call type arguments, or `schema_of(T)` are no
  longer reported as unused.
