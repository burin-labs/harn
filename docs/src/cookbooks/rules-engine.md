# Rule engine cookbook — scan, lint, and codemod

The Harn **rule engine** matches and rewrites code *structurally* (by syntax
tree), not by regex. A rule is a small TOML file; you run it read-only with
`harn scan`, or as a codemod with `harn codemod`. Under the hood it is the
`harn-rules` crate, exposed to `.harn` as `std/rules`.

> **Languages:** rules target a tree-sitter grammar: Harn, TypeScript/JS, Rust,
> Go, Python, Java, C/C++, Ruby, and more.

## How do I search for a code shape?

Use an inline pattern with `$VAR` holes. This finds every optional-chain +
nullish-coalesce site (the "destructure with defaults" shape):

```console
$ harn scan '$X?.$K ?? $D' src --lang typescript
src/config.ts:12:18: cfg?.timeout ?? 30   [D=30 K=timeout X=cfg]
src/config.ts:13:18: cfg?.retries ?? 3    [D=3 K=retries X=cfg]
2 match(es) in 1 file(s)
```

The same structural search works over Harn source:

```console
harn scan '$X?.$K ?? $D' crates/harn-stdlib --lang harn
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

For Harn source, `[[where]]` can also filter by resolved binding identity or
capture type. This distinguishes two same-named call sites by the declaration
they actually resolve to:

```toml
id = "global-target-call"
language = "harn"

[rule]
pattern = "$FN($ARG)"

[[where]]
metavar = "FN"
resolvesTo = { name = "target", kind = "fn", line = 1 }

[[where]]
metavar = "ARG"
type = "int"
```

The JSON output keeps `captures` as text and adds `capture_metadata` with
`resolved` and `type` entries when the Harn resolver can supply them.

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
Point `--rule-pack <pack>` at a directory or installed package to run its
top-level `*.toml` rules.
Installed packages work by name, and built-in packs live under `std/rules`.

```console
harn codemod --rule-pack std/rules/destructure-defaults src --apply
```

The built-in `destructure-defaults` pack handles Harn statement runs such as
`let x = input?.x ?? d` / `let alias = input?.field ?? d`, where a sequence fold
needs more context than a single-node TOML rule can express.

## How do I run my project's rules without naming them?

Declare the rule directories in your `harn.toml` and `harn scan` / `harn codemod`
discover them automatically:

```toml
# harn.toml
[rules]
ruleDirs = ["rules"]
```

```console
harn scan src             # runs every rule under rules/ over src/
harn codemod src          # applies the codemod rules; lints are skipped
```

With `[rules] ruleDirs` set, `harn scan <paths>` needs no inline pattern — a
pattern is signalled by `--lang`, so its absence means "use the project's
rules." `harn codemod` applies only the rules that have a `fix`; lint/search
rules in the same pack are ignored. Paths are resolved relative to the
`harn.toml` directory. Each `ruleDirs` entry loads top-level `*.toml` files;
put utility rules or fixtures in nested directories unless the manifest names
that directory explicitly.

## How do I publish and install a rule pack?

A rule pack is a Harn package that declares its rule directories:

```toml
[package]
name = "acme-rules"
version = "0.1.0"

[rules]
ruleDirs = ["rules"]
```

Publish it through the rule-specific package alias:

```console
harn rule publish --registry-name @acme/rules
harn rule search acme
harn add @acme/rules@0.1.0
harn scan --rule-pack @acme/rules src
```

`harn rule publish` uses the same tag + package-index PR flow as `harn
publish`, but first validates the rule files and writes rule-pack metadata to
the registry index. `harn rule search` lists only rule packs and includes the
pack description, languages, rule count, and safety summary. After `harn add`,
`--rule-pack` accepts either the dependency alias or the canonical registry name
recorded in `harn.lock`.

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

## How do I write a custom lint rule in Harn?

For a project convention that a declarative rule can't capture, author an
imperative rule in Harn — the ESLint-plugin equivalent. Drop a `*.lint.harn`
module into a `ruleDirs` directory; it exports `lint(source)` and returns either
a finding, a list of findings, or a `rules_diagnostics(...)` result:

```harn,ignore
// rules/no-todo.lint.harn
pub fn lint(source) -> list {
  if source.contains("TODO") {
    return [{column: 1, line: 1, message: "TODO markers are banned", severity: "error"}]
  }
  return []
}
```

```console
harn lint src             # discovers rules/*.lint.harn and runs them per file
```

`harn lint` discovers these alongside the built-in rules and merges their
findings into the normal output — same exit code, same `--json` report, same
`disable` filtering. A finding is a dict with a required `message` plus optional
`severity` (`"error"`/`"warning"`/`"info"`, default `"warning"`), `line` /
`column` (default `1`), and `start_byte` / `end_byte`.

A script rule can also delegate to the structural engine and return the
`rules_diagnostics(...)` result directly:

```harn,ignore
import { rules_diagnostics } from "std/rules"

pub fn lint(source) {
  let rule = "id = \"no-foo\"\nlanguage = \"harn\"\nmessage = \"no foo\"\n[rule]\npattern = \"foo()\"\n"
  return rules_diagnostics({language: "harn", rule: rule, source: source})
}
```

Rules run in a read-only sandbox — the language, stdlib, and the structural rule
engine, but no filesystem / network / process access. A buggy rule **fails
safe**: a load error, runtime throw, or malformed return becomes a diagnostic
attributed to the rule, never a linter crash.

## How do I load a native lint rule library?

Native lint rules are the trusted-code escape hatch for rare cases that need
compiled Rust. Build a dynamic library that exports
`harn_native_lint_register_v1`, then point `harn.toml` at the directory that
contains the library:

```toml
[rules]
nativeRuleDirs = ["native-rules"]
```

```console
harn lint src             # loads native-rules/*.dylib|*.so|*.dll for this platform
harn lint --fix src       # applies native rule fixes through the normal lint path
```

The native ABI lives in `harn_lint::native`. A library registers one or more
`HarnNativeRuleDescriptor` values and emits `HarnNativeDiagnostic` values with
the same message, severity, span, suggestion, and fix fields used by built-in
and declarative lint rules. Diagnostics carry the registered rule id, so
`[lint] disabled = ["your-rule-id"]` and `[lint.severity]` overrides work the
same way.

Loading native code is an explicit trust decision. Harn only loads libraries
from configured `nativeRuleDirs`; it does not search environment variables,
global plugin paths, or package caches.

## See also

- [Structured refactorings cookbook](./structured-refactorings.md) — the
  AST-edit primitives the engine builds on.
- [Destructure with defaults cookbook](./destructure-with-defaults.md) — the
  flagship codemod in depth.
- The `harn-rules` skill (`harn skills get harn-rules --full`).
