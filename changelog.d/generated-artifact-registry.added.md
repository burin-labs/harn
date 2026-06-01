- **Single source of truth for generated-artifact drift checks.**
  `scripts/generated_artifacts.toml` now registers every "source of truth ->
  generated file + drift check" pair in one place, and `make
  check-generated-registry` fails the build when the registry disagrees with
  the Makefile `all:` recipe, the CI workflows, or the declared output files.
  A new `gen-*`/`check-*` pair can no longer silently skip CI. Added a
  cross-crate `make check-tree-sitter-keywords` guard (with `make
  gen-tree-sitter-keywords`) so the tree-sitter grammar's reserved-word set
  cannot drift from the lexer `KEYWORDS` const. Both guards run in CI and in
  the pre-commit / pre-push hooks.
