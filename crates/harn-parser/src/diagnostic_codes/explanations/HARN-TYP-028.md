# HARN-TYP-028 — declared parameter has no type annotation

A parameter with no type annotation is unchecked in both directions. The body
may reach for any member on it, and every caller may pass any value. Nothing
recovers the type later, so the mistake shows up at run time as a failed member
access instead of at check time with a code and a repair.

The rule covers named declarations: `fn`, `pub fn`, `gen fn`, `pipeline`,
`tool`, methods in an `impl` block, and signatures in an `interface` block. A
default value does not exempt a parameter, because the default constrains only
the call that omits the argument.

Closure and lambda parameters are not covered. The checker types those from the
position the literal appears in, so they are inferred rather than implicit.

## Fix it

Annotate the parameter:

```harn
fn greet(user: {name: string}) -> string { return "hi ${user.name}" }
```

When a value really is unconstrained, say so:

```harn
fn passthrough<T>(value: T) -> T { return value }
```

Use a generic when input and output have the same shape. Use `unknown` at a
genuine dynamic boundary, then narrow or validate it before use.

## Migrate a codebase

`harn fix --apply --code HARN-TYP-028 <path>` infers each parameter's type from
how the body uses it and from the arguments at every call site in the module
graph, writes the annotation, and reports how many parameters it could not
prove anything about. Those fall back to `unknown` for a human to refine.

For an unattended runtime-version migration of code written before this rule,
use `harn fix --apply --safety behavior-preserving --preserve-implicit-any
--json <path>`. That compatibility mode writes only explicit `any`, preserving
the former unchecked call contract. Its census fails if any eligible parameter
is unresolved or receives a narrower annotation. It does not replace the
surface-changing authoring repair above.
