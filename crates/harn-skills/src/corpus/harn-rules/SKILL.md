---
name: harn-rules
short: Structural search, lint rules, and codemods with the Harn rule engine.
description: Use for authoring and running declarative structural rules — patterns, lint diagnostics, and codemods.
when_to_use: Use when writing or running a structural rule, a `harn scan` pattern, or a `harn codemod` fix.
---

# Harn rule engine

Use this skill when matching or rewriting code structurally — a search pattern,
a lint rule, or a codemod — instead of with regex/glob. The engine is the
`harn-rules` crate; it is exposed to Harn as `std/rules` and to the CLI as
`harn scan` (read-only) and `harn codemod` (apply).

Pair it with [[harn-language]] for `.harn` syntax and [[harn-testing]] for
conformance fixtures.

## Start here

- `crates/harn-rules/README.md` is the rule-model reference (matcher forms,
  relational/composite algebra, `where`/`transform`/`fix`, safety tiers).
- `docs/src/cookbooks/rules-engine.md` is the worked-example cookbook
  (`harn scan` / `harn codemod`, authoring a rule).
- A rule is **TOML**; an agent can author and run one entirely from `.harn`
  (via `std/rules`) without recompiling the binary.

## A rule, end to end

- A rule has an identity (`id`, `language`, `severity`, `message`), a `[rule]`
  matcher block, and an optional `fix`.
- **Scalar fields come before `[rule]`** — `[rule]` opens a TOML table, so any
  later top-level scalar would bind to it.
- The rule's kind is derived: a `fix` makes it a **codemod**; a `message` with
  no `fix` is a **lint**; a bare matcher is a **search**.

## Matcher forms (the `[rule]` block)

- `pattern` — a code snippet with `$VAR` metavariable holes (ast-grep style),
  compiled to a tree-sitter query. Operators/keywords match literally
  (`??` ≠ `||`); a repeated `$VAR` unifies; a metavar-free pattern is a
  **literal** pattern (`foo()` matches calls to `foo`, not any call).
- `$VAR:kind` — a **typed placeholder**: bind only nodes of a syntactic class.
  `expr`/`stmt`/`type`/`ident` aliases resolve to the grammar's supertype
  (TypeScript/JS/Python), else use an exact tree-sitter kind. Unknown kind =
  compile error.
- `kind` — a bare tree-sitter node kind. `regex` — a regex over node text.
- **Relational** (each a sub-rule, tuned by `stopBy`/`field`): `inside`
  (ancestor), `has` (descendant), `follows`/`precedes` (siblings).
- **Composite**: `all`/`any` (lists), `not` (a sub-rule), `matches` (a
  `[utils.NAME]` utility rule by id). Every key on a node is ANDed.

## Predicates, rewrite, and safety

- A `where` predicate narrows matches: `regex`, `comparison = { op, value }`,
  a recursive `pattern`, or Harn-only semantic filters
  `resolvesTo = { name, kind, line, column, id }` and `type = "int"`.
  Harn match dictionaries keep text in `captures` and add
  `capture_metadata` with `resolved` / `type` where available.
- `[transform.NAME]` synthesizes a metavar before fixing (`convert = "snake"`,
  `replace = { regex, by }`, `substring = { start, end }`).
- `fix` interpolates `$VAR` / `${VAR}` (`$$` → `$`) and splices
  format-preserving edits.
- `safety` (least→most dangerous): `format-only` → `behavior-preserving` →
  `scope-local` (default) → `surface-changing` → `capability-changing` →
  `needs-human`. The two safest are **machine-applicable**; the rest are
  **suggestions** (opt-in). `auto_apply` refuses anything riskier;
  `apply_checked` also fails a non-idempotent fix.

## From the CLI

- `harn scan '<pattern>' <paths> --lang <lang>` — structural search; also
  `--rule <file>` / `--rule-pack <dir>`, `--report-only` (per-file counts),
  `--json`.
- `harn codemod --rule <file> <paths>` — **dry-run by default** (a unified diff
  per file with safety + idempotency); `--apply` writes (capability-gated),
  `--allow-unsafe` applies above machine-applicable safety, `--json`.
- `harn lint <paths>` — runs project lint rules. Declarative `language = "harn"`
  rules and imperative `*.lint.harn` modules (a `pub fn lint(source) -> list` of
  `{message, severity?, line?, column?}` findings) in `[rules] ruleDirs` merge
  into the normal output. `.harn` rules run read-only and fail safe.

## From `.harn` (`std/rules`)

- `rules_search({rule, source|paths, language})` → matches with capture
  bindings. `rules_report(...)` → a report-only data table (counts + rows).
- `rules_diagnostics(...)` → declarative diagnostics. `rules_apply({..., dry_run,
  allow_unsafe})` → codemod (gated; `dry_run` by default).
- `rules_visit({rule, ..., on_match: fn(node, ctx) { ... }})` — the imperative
  escape hatch: the visitor returns its report(s) (`nil`/`false` to skip,
  `true` to flag, a `{message, fix, safety, severity}` dict, or a list).

## Gotchas

- Harn source is supported: pass `--lang harn`, set `language = "harn"`, or let
  `.harn` paths resolve from their extension.
- `rules_apply` is a gated deterministic tool: call
  `hostlib_enable("tools:deterministic")` before it (even for a dry run).
- Format every rule run / fixture with both `cargo fmt` and `harn fmt`.
