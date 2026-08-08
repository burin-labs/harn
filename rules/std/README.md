# `std` rule pack (seed)

A small, curated starter pack of structural rules for the Harn
[rule engine](../../crates/harn-rules) (#2844) — the engine's reference corpus
and dogfood. Run them with `harn scan` / `harn codemod`:

```console
harn scan --rule-pack rules/std src
harn codemod --rule rules/std/destructure-defaults.toml src      # dry-run
```

## Rules

| Rule | Kind | Safety | What |
|------|------|--------|------|
| `destructure-defaults` | codemod | `scope-local` (suggestion) | Collapse `const x = obj?.x ?? d` into `const { x = d } = obj` |
| `no-console-log` | lint | — | Flag stray `console.log(...)` calls |

### `destructure-defaults`

The flagship — powers the #2824 / burin-code#1629 migration. The unified `$K`
means it only rewrites the **non-alias** case (binding name == property name);
aliases (`const t = cfg?.timeout ?? d`) are left alone.

It is a **suggestion** (`scope-local`, not auto-applied) because the rewrite
assumes `$X` is non-nullish: `obj?.x ?? d` yields `d` when `obj` is
null/undefined, but `const { x = d } = obj` throws on a null `obj`. Apply it
where `$X` is a known-present object, as in the #2824 sites.

## Testing

Each rule ships with an annotation fixture (`<rule>.ts`, the Semgrep
`// ruleid:` / `// ok:` convention) for `harn rule test` (#2842):

```console
harn rule test rules/std
```

The rules are also covered by `crates/harn-rules/tests/harn_rules/seed_pack.rs`, which
loads each shipped `*.toml` and asserts its behavior (a CI gate today, before
`harn rule test` lands on `main`).

## Not yet here

Ports of the hardcoded `harn-lint` rules (`optional-shorthand`,
`redundant-nil-ternary`, `import-order`, …) target **`.harn`** source, which
needs a Harn tree-sitter grammar — tracked in #2888. Until then this pack
targets TypeScript/JS and the other grammars the engine already ships.
