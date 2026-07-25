- Delegate runtime schema constraint evaluation to the standards-based
  `jsonschema` Draft 2020-12 implementation while preserving Harn types,
  recursive defaults, bounded traversal, path-aware diagnostics, and bounded
  compiled-validator reuse. The internal runtime-limit key is now
  `max_schema_validator_cache_entries`. Canonical Harn unions now export as
  JSON Schema `anyOf`, preserving their inclusive match semantics when branches
  overlap.
