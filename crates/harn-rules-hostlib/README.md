# harn-rules-hostlib

The host capability that exposes the [`harn-rules`](../harn-rules) declarative
rule engine to the Harn language. Part of the
[Rule Engine program](https://github.com/burin-labs/harn/issues/2826)
(Epic A, [harn#2838](https://github.com/burin-labs/harn/issues/2838)).

## Why a separate crate?

`harn-rules` depends on `harn-hostlib` (for the tree-sitter grammars), so the
`rules` builtins **cannot** live inside `harn-hostlib` — that would be a
dependency cycle. This crate sits one level up
(`harn-cli` → `harn-rules-hostlib` → `harn-rules` → `harn-hostlib`) and an
embedder calls [`install`] next to `harn_hostlib::install_default`.

## Builtins (`std/rules` wraps these)

| Builtin | Gate | What |
|---|---|---|
| `rules.search` | read-only | run a rule, return matches with capture bindings |
| `rules.report` | read-only | run report-only, return a `DataTable` (counts + rows) |
| `rules.apply`  | write (deterministic-tools) | apply a codemod `fix`; dry-run by default, safety-gated |

A rule is passed as its **TOML source** (`rule`), with either inline `source`
(+ `language`) or a list of `paths`. So an agent can author and run a rule
entirely from `.harn` without recompiling the binary.

## Not yet here

The **imperative** rule form — a `.harn` module exporting an
`on_match($node, ctx)` visitor — needs a synchronous closure-callback from a
Rust builtin, which the VM does not support today (only async builtins can
call back into `.harn`). That requires a VM-core prerequisite and is tracked
separately.
