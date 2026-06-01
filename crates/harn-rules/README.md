# harn-rules

The declarative structural rule engine for Harn — the Rust core behind
`harn rules` / lint / codemod surfaces. Part of the
[Rule Engine program](https://github.com/burin-labs/harn/issues/2826)
(Epic A, [harn#2827](https://github.com/burin-labs/harn/issues/2827)).

A **rule** says *what to match* and optionally *how to rewrite* it. The
engine compiles the rule against the tree-sitter machinery in `harn-hostlib`
and produces matches with metavariable bindings — the structural complement
to regex/glob search.

This crate ships the **atomic matching tier**
([harn#2832](https://github.com/burin-labs/harn/issues/2832)). Relational /
composite matching (#2833) and `where` / `transform` / `fix` interpolation
(#2834) build on it.

## Rule shape (TOML)

```toml
id = "destructure-with-defaults"
language = "typescript"
severity = "warning"                 # info | warning (default) | error
message = "Collapse `?.x ?? default`"
fix = "{ $KEY: $SRC }"               # presence makes the rule a codemod

[rule]                               # the matcher block — keep it LAST
pattern = "$SRC?.$KEY ?? $DEFAULT"   # one of: pattern | kind | regex
```

> **Key ordering:** because `[rule]` opens a TOML table, every scalar field
> (`id`, `language`, `severity`, `message`, `fix`) must appear **before** it.

A rule's kind is derived from its shape: a `fix` makes it a **codemod**; a
`message` with no `fix` makes it a **lint**; a bare matcher is a **search**.

### Atomic matcher forms

- `pattern` — a code snippet in the target grammar with `$VAR` metavariable
  holes. Compiled to a tree-sitter query: each `$VAR` becomes a capture, the
  snippet's operators/keywords are matched literally (so `??` ≠ `||`), and a
  repeated `$VAR` unifies (must bind identical text). Variadic `$$$` holes
  land with the relational tier (#2833).
- `kind` — a bare tree-sitter node kind (e.g. `"call_expression"`).
- `regex` — a regular expression over the source text.

## Usage

```rust
use harn_rules::{Rule, CompiledRule};

let rule = Rule::from_toml_str(/* … */)?;
let compiled = CompiledRule::compile(&rule)?;
for m in compiled.run(source)? {
    println!("{} at {:?}: {}", m.rule_id, m.span, m.text);
    for (name, binding) in &m.bindings {
        println!("  ${name} = {}", binding.text);
    }
}
```

Load from disk with `load_rule_file(path)` or `load_rule_dir(dir)`.
