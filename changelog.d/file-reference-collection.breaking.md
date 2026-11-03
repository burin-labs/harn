- `harn_modules::ReferenceIndex::file_edges()` was removed. File-level reference
  graphs now collect edges directly without constructing a positional index.

  Migration: replace `index.file_edges()` with
  `harn_modules::file_reference_edges(&graph, &sources)`. Retain the module graph
  and parsed sources used to build the positional index, or rebuild them with
  `harn_modules::build_for_reference_index` when they are no longer available.
