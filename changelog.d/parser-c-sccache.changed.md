- CI: cc-crate build-script compiles (notably tree-sitter-harn's ~22 MB
  generated `parser.c`) now route through the same health-gated sccache as
  rustc, so warm lanes stop repaying the C compile on every run.
