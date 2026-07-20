- `harn_parser::diagnostic::edit_distance` has been removed. It was a plain
  Levenshtein implementation with no in-tree callers outside the ranking wrapper
  that replaced it; embedders needing the same metric should depend on `strsim`
  directly, which is what the crate now uses internally.
  `harn_parser::diagnostic::find_closest_match` — the suggestion API that actually
  encodes Harn's ranking policy — is unchanged.
