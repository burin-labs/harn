<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

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

## Specification chapters

The language specification is organized into the chapters below.

- [Lexical rules](./spec/language/01-lexical-rules.md)
- [Grammar](./spec/language/02-grammar.md)
- [Operator precedence table](./spec/language/03-operator-precedence-table.md)
- [Scope rules](./spec/language/04-scope-rules.md)
- [Destructuring patterns](./spec/language/05-destructuring-patterns.md)
- [Evaluation order](./spec/language/06-evaluation-order.md)
- [Runtime values](./spec/language/07-runtime-values.md)
- [Binary operator semantics](./spec/language/08-binary-operator-semantics.md)
- [Control flow](./spec/language/09-control-flow.md)
- [Concurrency](./spec/language/10-concurrency.md)
- [Pipeline lifecycle](./spec/language/11-pipeline-lifecycle.md)
- [Error model](./spec/language/12-error-model.md)
- [Functions and closures](./spec/language/13-functions-and-closures.md)
- [Enums](./spec/language/14-enums.md)
- [Structs](./spec/language/15-structs.md)
- [Impl blocks](./spec/language/16-impl-blocks.md)
- [Interfaces](./spec/language/17-interfaces.md)
- [Attributes](./spec/language/18-attributes.md)
- [Type annotations](./spec/language/19-type-annotations.md)
- [Built-in methods](./spec/language/20-built-in-methods.md)
- [Iterator protocol](./spec/language/21-iterator-protocol.md)
- [Method-style builtins](./spec/language/22-method-style-builtins.md)
- [Runtime errors](./spec/language/23-runtime-errors.md)
- [OAuth](./spec/language/24-oauth.md)
- [Persistent store](./spec/language/25-persistent-store.md)
- [Checkpoint & resume](./spec/language/26-checkpoint-resume.md)
- [Agent lifecycle (suspend/resume)](./spec/language/27-agent-lifecycle-suspend-resume.md)
- [Host shell discovery](./spec/language/28-host-shell-discovery.md)
- [Workspace manifest (`harn.toml`)](./spec/language/29-workspace-manifest-harn-toml.md)
- [Sandbox mode](./spec/language/30-sandbox-mode.md)
- [Test framework](./spec/language/31-test-framework.md)
- [Environment variables](./spec/language/32-environment-variables.md)
- [Known limitations and future work](./spec/language/33-known-limitations-and-future-work.md)
