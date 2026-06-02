# Rule engine cookbook — scan, lint, and codemod

The Harn **rule engine** matches and rewrites code *structurally* (by syntax
tree), not by regex. A rule is a small TOML file; you run it read-only with
`harn scan`, or as a codemod with `harn codemod`. Under the hood it is the
`harn-rules` crate, exposed to `.harn` as `std/rules`.

> **Languages:** rules target a tree-sitter grammar — TypeScript/JS, Rust, Go,
> Python, Java, C/C++, Ruby, and more. There is no Harn grammar yet, so `.harn`
> source can't be scanned structurally ([#2888](https://github.com/burin-labs/harn/issues/2888)).

## How do I search for a code shape?

Use an inline pattern with `$VAR` holes. This finds every optional-chain +
nullish-coalesce site (the "destructure with defaults" shape):

```console
$ harn scan '$X?.$K ?? $D' src --lang typescript
src/config.ts:12:18: cfg?.timeout ?? 30   [D=30 K=timeout X=cfg]
src/config.ts:13:18: cfg?.retries ?? 3    [D=3 K=retries X=cfg]
2 match(es) in 1 file(s)
```

- `$X`, `$K`, `$D` are **metavariables** — each binds a sub-tree and is printed
  in `[...]`. A repeated `$X` must bind identical text.
- Add `--report-only` for per-file counts instead of each match, or `--json`
  for a machine envelope.
- Narrow a hole to a syntactic class with a **typed placeholder**:
  `harn scan 'log($A:identifier)' src --lang typescript` matches `log(x)` but
  not `log(f())`.

## How do I write a reusable rule?

Put the matcher in a TOML file. Scalars (`id`, `language`, `message`, `fix`,
`safety`) come **before** the `[rule]` table:

```toml
# destructure-defaults.toml
id = "destructure-defaults"
language = "typescript"
message = "Collapse `?.x ?? default` into a destructure with a default"
fix = "{ $K = $D } = $X"          # presence of `fix` makes this a codemod
safety = "behavior-preserving"    # → machine-applicable

[rule]
pattern = "$X?.$K ?? $D"
```

Run it read-only with `harn scan --rule destructure-defaults.toml src`, or
apply it (below). A rule with a `message` but no `fix` is a **lint**; a bare
matcher is a **search**.

See `crates/harn-rules/README.md` for the full model: relational keys
(`inside` / `has` / `follows` / `precedes`), composite keys (`all` / `any` /
`not` / `matches`), `[[where]]` predicates, and `[transform.NAME]`.

## How do I apply a codemod?

`harn codemod` is **dry-run by default** — it prints a unified diff per file
and writes nothing:

```console
$ harn codemod --rule destructure-defaults.toml src
would change src/config.ts  [safety=BehaviorPreserving, idempotent=true]
--- before
+++ after
@@ -12,2 +12,2 @@
-const timeout = cfg?.timeout ?? 30;
+const { timeout = 30 } = cfg;
...
1 file(s) would change (dry run; pass --apply to write)
```

Pass `--apply` to write. Applying is **capability-gated** and respects the
rule's `safety`: only `format-only` and `behavior-preserving` fixes apply
automatically; anything riskier needs `--allow-unsafe`.

```console
$ harn codemod --rule destructure-defaults.toml src --apply
rewrote src/config.ts  [safety=BehaviorPreserving]
1 file(s) rewritten (1 changed)
```

Re-running a folded file changes nothing — fixes are checked for idempotency.
Point `--rule-pack <dir>` at a directory to run every `*.toml` rule in it.

## How do I run a rule from `.harn`?

`std/rules` is the same engine, callable inline — an agent can author and run a
rule without recompiling:

```harn,ignore
import { rules_search, rules_apply } from "std/rules"

let rule = "id = \"calls\"\nlanguage = \"typescript\"\n[rule]\npattern = \"$FN()\"\n"

let found = rules_search({rule: rule, source: "foo();\nbar();\n", language: "typescript"})
__io_println(found.match_count)            // 2

// rules_apply is a gated deterministic tool; dry-run by default.
hostlib_enable("tools:deterministic")
let result = rules_apply({rule: codemod_rule, paths: ["src/a.ts"], dry_run: false})
```

For logic a declarative rule can't express, `rules_visit({rule, ..., on_match:
fn(node, ctx) { ... }})` calls a visitor per match; the visitor *returns* its
report(s) (`nil`/`false` to skip, a `{message, fix, safety}` dict, or a list).

## See also

- [Structured refactorings cookbook](./structured-refactorings.md) — the
  AST-edit primitives the engine builds on.
- [Destructure with defaults cookbook](./destructure-with-defaults.md) — the
  flagship codemod in depth.
- The `harn-rules` skill (`harn skills get harn-rules --full`).
