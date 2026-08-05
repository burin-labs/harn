# Harn language specification

Version: tracks the workspace `0.8.x` series; derived from the
implementation and updated alongside it. The language is still
pre-1.0 — surface-level breaking changes are possible between minor
releases. See the
[changelog](https://github.com/burin-labs/harn/blob/main/CHANGELOG.md) for
what changed and when, and the `Stability` column in subsections below for
per-feature guarantees.

Harn is a pipeline-oriented programming language for orchestrating AI agents.
It is implemented as a Rust workspace with a lexer, parser, type checker,
tree-walking VM, tree-sitter grammar, and CLI/runtime tooling. Programs consist of named pipelines
containing imperative statements, expressions, and calls to registered builtins
that perform I/O, LLM calls, and tool execution.

The canonical specification is authored as per-chapter Markdown files in
`spec/chapters/` (one file per top-level section). `spec/HARN_SPEC.md` is a
generated single-file assembly of those chapters — do not edit it directly —
and the hosted docs page `docs/src/language-spec.md` is generated alongside it
by `scripts/sync_language_spec.harn`. Edit the chapter files and run
`make sync-language-spec` (the pre-commit hook does this automatically).

