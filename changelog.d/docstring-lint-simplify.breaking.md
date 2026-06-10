Docstring lint requirements are now SOTA-default: the `missing-harndoc`
rule (HARN-LNT-024) is **opt-in** via `[lint] require_docstrings = true`
in `harn.toml` instead of warning on every undocumented `pub fn`, and the
stdlib metadata contract (HARN-STD-101) shrank from five required tags to
two — `@effects` + `@errors`. `@allocation` is retired entirely (parser
ignores it; the field is gone from `StdlibMetadata` and `harn graph
--json`), `@api_stability` is optional (absent ⇒ stable), and `@example`
is optional everywhere: LSP hover and `harn graph --json` now synthesize
a usage example from the type signature when no hand-written one exists
(`derived_example` in graph JSON, a labeled "derived from signature"
block in hover). ~2,900 lines of boilerplate tags were stripped from the
embedded stdlib (`scripts/strip_stdlib_metadata.harn`), and
`scripts/backfill_stdlib_metadata.harn` now backfills only the two
required fields.
