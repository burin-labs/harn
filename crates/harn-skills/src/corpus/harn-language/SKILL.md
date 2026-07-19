---
name: harn-language
short: Harn syntax, modules, types, diagnostics, and script structure.
description: Use for Harn language syntax, typechecking, modules, imports, and idiomatic script authoring.
when_to_use: Use when writing, reviewing, or explaining Harn source code and language-level behavior.
---

# Harn language

Use this skill when writing, reviewing, or explaining `.harn` source code.

Pair it with [[harn-testing]] for fixtures and [[harn-diagnostics]] for user-facing errors.

## Start here

- Read `docs/llm/harn-quickref.md` before editing `.harn` files.
- Use `docs/llm/harn-triggers-quickref.md` when trigger manifests are involved.
- Treat `spec/HARN_SPEC.md` as the canonical language reference.
- Default new script entrypoints to `fn main(harness: Harness) { ... }` and
  route side effects through `harness.*`.
- Use `harness.fs.mkdtemp_in_workspace(prefix?)` for scratch files that must be
  visible under sandbox policy; avoid composing `.harn/tmp` paths by hand.
- Use typed `std/fs` conditional replacement for shared state or artifacts
  that must not overwrite a newer observation; branch on the closed receipt.
- For connector credentials, read canonical ids such as
  `provider/access-token` through `harness.secrets` from a package script; the
  runner scopes the default provider from the nearest `harn.toml`.
- Edit `spec/HARN_SPEC.md`, not `docs/src/language-spec.md`, for spec changes.
- Regenerate generated spec docs with `make sync-language-spec`.
- Check user-visible behavior with conformance fixtures under `conformance/tests/`.
- Keep examples small enough for agents to copy without hidden setup.
- Prefer existing syntax and stdlib helpers over new host-side shortcuts.

## Syntax checklist

- Imports go first.
- `pipeline` is the usual entry-point construct.
- `fn` is for reusable pure or host-backed logic.
- `let` binds values; avoid mutation unless the language surface requires it.
- Use explicit type annotations at boundaries.
- Use `match` for variant-sensitive behavior.
- Use `for` for straightforward iteration.
- Use `parallel each` for bounded fan-out work.
- Use `parallel settle` when collecting settled outcomes matters.
- Always set `max_concurrent` on broad parallel work.
- Use triple-quoted strings for long prompts in Harn source.
- Heredoc syntax is for LLM tool-call argument JSON, not general strings.
- Keep comments factual and close to non-obvious logic.

## Types and boundaries

- Treat `unknown` as the type for untrusted inputs.
- Use `SchemaContract<T>` for deterministic cross-field rules after structural
  validation. Capture typed context in the rule closure; do not replace it with
  an open dictionary.
- Narrow `unknown` with `type_of`, `schema_is`, or validated helpers.
- Use `any` only as an explicit escape hatch.
- Do not erase types merely to silence the typechecker.
- Keep struct and enum names stable when they are user-facing.
- Add conformance coverage before changing type inference behavior.
- Preserve diagnostic codes and spans when refactoring typechecker logic.
- Keep imported callable declarations aligned with module graph behavior.
- Watch for strict-types behavior when changing boundary APIs.
- Update examples when public type syntax changes.

## Modules and imports

- Keep import paths explicit and readable.
- Avoid relying on the current working directory in examples.
- When touching module resolution, inspect `crates/harn-modules`.
- Cross-file checks should use the same module graph as the CLI.
- Public symbols should remain discoverable by editor tooling.
- Avoid duplicate import spellings for the same file.
- Prefer canonical relative paths in fixtures.
- Add a cycle regression when changing import traversal.
- Keep generated docs and tree-sitter grammar in sync after syntax changes.
- Use [[harn-orchestration]] when module behavior affects workflows.

## Prompt templates

- The template engine lives in `crates/harn-vm/src/stdlib/template.rs`.
- Do not add a second prompt-template parser.
- Keep host-call and script-call rendering behavior identical.
- Preserve pre-v2 `{{name}}` missing-identifier passthrough.
- New template constructs should fail with useful parse diagnostics.
- Update `docs/src/prompt-templating.md` for template syntax changes.
- Update `docs/llm/harn-quickref.md` when agents need the new syntax.
- Update VS Code grammar when `.harn.prompt` syntax changes.
- Add conformance fixtures under `conformance/tests/template_*`.
- Use [[harn-diagnostics]] for template parse and lint diagnostics.

## Simpler first

- Can this be ordinary Harn instead of Rust?
- Can a stdlib helper express this without a new language form?
- Can the typechecker infer it without a new annotation?
- Can a conformance fixture capture the behavior without a large harness?
- Can docs describe the rule in one sentence?
- Can the formatter preserve the style without special cases?
- Can the linter derive the rule from existing AST structure?
- Can the tree-sitter grammar remain a direct mirror of parser syntax?
- Can existing examples be updated mechanically?
- Can user-visible behavior stay backward compatible?

## Verify

- Narrow script check: `cargo run --quiet --bin harn -- check <path>`.
- Narrow lint check: `cargo run --quiet --bin harn -- lint <path>`.
- Narrow format check: `cargo run --quiet --bin harn -- fmt --check <path>`.
- Narrow conformance case: `cargo run --quiet --bin harn -- test conformance --filter <name>`.
- Parser changes: `cargo test -p harn-parser <filter>`.
- Formatter changes: `cargo test -p harn-fmt <filter>`.
- Linter changes: `cargo test -p harn-lint <filter>`.
- Syntax or keyword changes: `make conformance`.
- Harn fixture changes: `make lint-harn` and `make fmt-harn`.
- Tree-sitter changes: `(cd tree-sitter-harn && npm test)`.
