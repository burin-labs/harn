- Serialized dependency materialization under `.harn/packages/` so concurrent
  Harn commands do not race while removing and copying the same installed
  package tree.
- Kept the prompt-prose ratchet out of the generic Rust pre-commit path so
  package-only edits do not build `harn` just to commit.
