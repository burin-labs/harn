# Migrating to the `const`/`let` keyword scheme

Harn's variable-binding keywords now follow the TypeScript / Swift
convention. This is a breaking change to every `.harn` source file.

| Before | After | Meaning |
|---|---|---|
| `let x = …` (immutable) | `const x = …` | **immutable** binding (the default) |
| `var x = …` (mutable) | `let x = …` | **mutable** binding (reassignable) |
| `const NAME = …` (compile-time) | `const NAME = …` | unchanged spelling — see below |
| `var` keyword | — | **removed**; using it is a compile error with a migration hint |

The old scheme *inverted* the universal JS/TS meaning: `let` was immutable
and `var` was the mutable form. Now `const` is the immutable default and `let`
is the mutable one, matching what every editor, linter, and code model
expects.

## Semantics

- **`const` means this binding's value never changes.** It is the common case —
  reach for it by default and use `let` only when the value must change.
- **`let` is a mutable binding.** Reassignment and field/index mutation are
  allowed (`let o = {}; o.a = 1`).
- **The keyword *spelling* follows TypeScript; the *rule* follows Swift.** The
  two agree on reassignment but disagree on collection contents, so it is worth
  being exact. TypeScript's `const o = {}; o.a = 1` is legal, because a TS
  object is a *reference* and `const` constrains only the binding. Harn's
  collections are **values**, so `o.a = 1` changes `o`'s whole value and
  requires `let` — the same position Swift takes for the same reason. Methods
  such as `push` return a new value and modify nothing, so they remain fine on
  a `const`. See [Binding mutability](../language-spec.md) for the full rule.
- **`const` now accepts any initializer.** Previously `const` was a strict
  compile-time constant that *rejected* impure or non-foldable initializers.
  Because `const` is now the default immutable binding, that restriction is
  gone: `const user = fetch_user()` is fine. When the initializer happens to
  be in the pure const-eval subset it is still folded at compile time, but
  this is a transparent optimization — it never changes observable behavior,
  and an impure or erroring initializer is simply not folded (it is not a
  compile error). `const z = 1 / 0` errors at runtime, exactly like
  `let z = 1 / 0`.
- **`var` is removed.** It is retained as a reserved word only so that using
  it produces a clear migration diagnostic pointing at `let`/`const`.

### Before

```harn,ignore
let name = "ada"       // immutable — old `let` was the immutable keyword
var count = 0          // mutable — old `var` was the mutable keyword
count = count + 1
```

### After

```harn,ignore
const name = "ada"     // immutable
let count = 0          // mutable
count = count + 1
```

## Automated migration

The rename is fully mechanical. Use `harn codemod` with these two rules, run
in order (`let`→`const` first, then `var`→`let`):

```toml
# 01-let-to-const.toml
id = "harn-let-to-const"
language = "harn"
fix = "const"
fixTarget = "kw"
[rule]
query = '(let_binding "let" @kw) @__match'
```

```toml
# 02-var-to-let.toml
id = "harn-var-to-let"
language = "harn"
fix = "let"
fixTarget = "kw"
[rule]
query = '(var_binding "var" @kw) @__match'
```

```sh
harn codemod --apply --allow-unsafe --rule 01-let-to-const.toml .
harn codemod --apply --allow-unsafe --rule 02-var-to-let.toml .
```

These rules rewrite only the binding keyword — type annotations, destructuring
patterns, and initializers are preserved byte-for-byte, and text inside strings
and comments is never touched. Run `let`→`const` first: running `var`→`let`
first would let the first rule re-match the freshly-minted `let`s and wrongly
turn mutable bindings into `const`.
