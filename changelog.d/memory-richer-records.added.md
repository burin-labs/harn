- Extended `std/memory` with mutable record metadata. `memory_store` now accepts
  `options.status`, `options.scope`, and `options.flags` (a `{name: bool}` map); records
  written without them stay byte-identical to existing logs. New `memory_update(namespace,
  id, patch, options?)` appends an in-place, projection-time overlay by record id (value,
  tags, status, scope, flags, provenance) while keeping the log append-only, and
  `memory_list(namespace, options?)` enumerates active records newest-first with
  `status` / `scope` / `tag` / `flag` filters. "Rejected" and similar states are just a
  `status`, so soft-retired records stay queryable.
