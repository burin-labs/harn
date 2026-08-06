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
| `rules.diagnostics` | read-only | run a declarative rule, return its diagnostics |
| `rules.visit` | read-only (async) | imperative `on_match($node, ctx)` visitor |
| `rules.apply`  | write (deterministic-tools) | apply a codemod `fix`; dry-run by default, safety-gated |

A rule is passed as its **TOML source** (`rule`), with either inline `source`
(+ `language`) or a list of `paths`. So an agent can author and run a rule
entirely from `.harn` without recompiling the binary.

## The imperative visitor (`rules.visit`)

[harn#2878](https://github.com/burin-labs/harn/issues/2878) — the native
escape hatch. `rules.visit` runs a rule's matcher and calls a `.harn` visitor
`on_match($node, $ctx)` once per match. The visitor **returns** its report(s)
— `nil`/`false` to skip, `true` to flag with rule defaults, a
`{message, fix, safety, severity}` dict, or a list of them — which become
diagnostics in the same shape `rules.diagnostics` emits. It can compute a
message or fix from the captured metavars, which the declarative form cannot.

Two design points worth knowing:

- **Async, registered directly on the VM.** A *synchronous* hostlib builtin
  cannot call a `.harn` closure (`Vm::call_closure_pub` is async-only), so
  `rules.visit` is an **async** builtin installed via
  `Vm::register_async_builtin` in [`install`] rather than through the sync
  `HostlibRegistry`. It obtains a child VM from its `AsyncBuiltinCtx` and calls
  back per match.
- **Returns rather than mutates.** `VmValue` has no callable variant
  carrying captured Rust state, so a mutating `ctx.report(...)` cannot be
  embedded in `ctx`. Returning the report is both the correct option and
  the simpler one.
