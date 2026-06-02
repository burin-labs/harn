- DRY / leaky-abstraction cleanup alongside the fixes above:
  - `harn-cli` now depends on the `hex` crate (as the rest of the workspace already does) and the two hand-rolled hex
    encoders (`registry::hex_bytes`, `skill_provenance::hex_encode`) were removed in favour of `hex::encode`.
  - `outline`'s `extract_rust` now delegates to the shared `extract_with_prefixes` helper instead of inlining a
    verbatim copy of its prefix-matching loop, matching every other per-language extractor.
  - `fs_watch` dropped its private re-implementation of `optional_string_list` and reuses `value_args::optional_string_list`.
  - Removed a dead, fully-subsumed early-return branch in the LSP `line_byte_range` helper.
