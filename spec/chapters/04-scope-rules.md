## Scope rules

Harn uses lexical scoping with a parent-chain environment model.

### Environment

Each `HarnEnvironment` has:

- A `values` dictionary mapping names to `HarnValue`
- A `mutable` set tracking which names were declared with `var`
- An optional `parent` reference

### Variable lookup

`env.get(name)` checks the current scope's `values` first, then walks up the `parent` chain.
Returns `nil` (which becomes `.nilValue`) if not found anywhere.

### Variable definition

- `let name = value` -- defines `name` as immutable in the current scope.
- `const NAME = value` -- defines `NAME` as immutable, with the
  initializer additionally folded at compile time by the bounded
  const-evaluator. Only pure expressions are accepted: literal
  arithmetic, string concatenation, literal lists/dicts, ternary /
  if-else, subscript access, and calls into a small whitelist of pure
  stdlib builtins (`len`, `format`, `min`, `max`, `abs`, `floor`,
  `ceil`, `round`, `lowercase`, `uppercase`, `trim`, `concat`, `join`).
  Any reference to `harness.*`, runtime constructs (`spawn`, `parallel`,
  `select`, `try`, `yield`, `emit`, `await`, …), user-defined
  functions, loops, or assignment is rejected with a `HARN-MET-001`,
  `HARN-CST-001`, `HARN-CST-002`, `HARN-CST-003`, or `HARN-CST-004`
  diagnostic depending on the failure mode. Issue #1791 carries the
  full design and rationale.
- `var name = value` -- defines `name` as mutable in the current scope.
- `var name = nil` -- leaves `name` widenable until the first non-`nil`
  assignment, which fixes the slot to `T | nil`. The explicit form
  `var name: T | nil = nil` remains valid when you want to pin `T`
  up front.
- `let _ = value` / `var _ = value` -- evaluate `value` and discard it
  without introducing a variable into scope. `_` can be reused any number
  of times in the same scope.

### Variable assignment

`name = value` walks up the scope chain to find the binding. If the binding is found but was
declared with `let`, throws `HarnRuntimeError.immutableAssignment`. If not found in any scope,
throws `HarnRuntimeError.undefinedVariable`.

### Scope creation

New child scopes are created for:

- Pipeline bodies
- `for` loop bodies (loop variable is mutable)
- `while` loop iterations
- `parallel`, `parallel each`, and `parallel settle` task bodies (isolated interpreter per task)
- `if`/`else` branch bodies and `match` arm bodies
- `try`/`catch` blocks (catch body gets its own child scope with optional error variable)
- Closure invocations (child of the *captured* environment, not the call site)
- `block` nodes

Control flow *headers* (`if` conditions, `match` scrutinees) evaluate in the current
scope, but each branch or arm body is its own child scope: bindings declared inside
shadow outer names and do not leak past the body.

