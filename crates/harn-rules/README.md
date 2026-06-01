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
([harn#2832](https://github.com/burin-labs/harn/issues/2832)) plus the
**predicate + rewrite layer**
([harn#2834](https://github.com/burin-labs/harn/issues/2834)). Relational /
composite matching (#2833) and the whole-project scan lifecycle (#2836)
build on it.

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

### `where` constraints, `transform`, and `fix`

A rule can narrow matches with `where` predicates, synthesize new metavars
with `transform`, and rewrite with `fix`:

```toml
id = "snakeify-getters"
language = "typescript"
fix = "$SNAKE()"                     # interpolates $VAR / ${VAR} (and $$ -> $)

[rule]
pattern = "$FN()"

[[where]]                            # keep only matches that pass every predicate
metavar = "FN"
regex = "^get[A-Z]"                  # or: comparison = { op = ">", value = 100 }
                                     # or: pattern = "..."  (recursive sub-pattern)

[transform.SNAKE]                    # derive a new metavar before fixing
source = "FN"
convert = "snake"                    # or: replace = { regex, by } / substring = { start, end }
```

`CompiledRule::apply(source)` runs the rule, drops matches that fail any
constraint, interpolates each match's `fix` (from its captured + transformed
metavars), and splices the replacements in — format-preserving, the same
byte-splice guarantee as `ast.batch_apply`. It returns the rewritten source
plus the per-match edits; the caller decides whether to write.

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
