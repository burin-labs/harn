# Harn AST node catalog

All AST nodes are variants of the `Node` enum, defined in
`crates/harn-parser/src/ast.rs`. Each node is wrapped in `Spanned<Node>`
(aliased as `SNode`) which pairs it with a source `Span` for diagnostics.

## Declarations

### `Pipeline`

```rust
Pipeline { name: String, params: Vec<String>, body: Vec<SNode>, extends: Option<String>, is_pub: bool }
```

A named pipeline declaration. `params` are the formal parameter names.
`body` is the list of statements. `extends` names the optional parent pipeline.

```harn
pipeline default(task, project) {
  let x = 1
}
```

### `LetBinding`

```rust
LetBinding { pattern: BindingPattern, type_ann: Option<TypeExpr>, value: Box<SNode> }
```

An immutable variable binding. `pattern` supports identifier or destructuring.

```harn
let result: string = compute()
let {x, y} = point
```

### `VarBinding`

```rust
VarBinding { pattern: BindingPattern, type_ann: Option<TypeExpr>, value: Box<SNode> }
```

A mutable variable binding.

```harn
var count: int = 0
```

### `OverrideDecl`

```rust
OverrideDecl { name: String, params: Vec<String>, body: Vec<SNode> }
```

An override declaration inside a child pipeline that extends a parent.

```harn
pipeline child(task) extends parent {
  override fill_strategy(target) {
    custom_fill(target)
  }
}
```

### `FnDecl`

```rust
FnDecl { name: String, type_params: Vec<TypeParam>, params: Vec<TypedParam>, return_type: Option<TypeExpr>, where_clauses: Vec<WhereClause>, body: Vec<SNode>, is_pub: bool }
```

Named function declaration with optional generics, typed parameters, return
type annotation, and where-clause constraints.

```harn
fn add(a: int, b: int) -> int {
  return a + b
}

fn process<T>(item: T) where T: Displayable {
  println(item.display())
}
```

### `ImportDecl`

```rust
ImportDecl { path: String }
```

Imports another `.harn` file by path.

```harn
import "shared/common.harn"
```

### `SelectiveImport`

```rust
SelectiveImport { names: Vec<String>, path: String }
```

Imports specific names from a module.

```harn
import { helper, util } from "shared/lib.harn"
```

### `TypeDecl`

```rust
TypeDecl { name: String, type_expr: TypeExpr }
```

A type alias declaration.

```harn
type UserId = string
type Pair = {first: int, second: int}
```

### `EnumDecl`

```rust
EnumDecl { name: String, variants: Vec<EnumVariant>, is_pub: bool }
```

An enum declaration. Each `EnumVariant` has a `name` and optional typed `fields`.

```harn
enum Color {
  Red
  Green
  Blue
  Custom(r: int, g: int, b: int)
}
```

### `StructDecl`

```rust
StructDecl { name: String, fields: Vec<StructField>, is_pub: bool }
```

A struct declaration. Each `StructField` has a `name`, optional `type_expr`,
and an `optional` flag.

```harn
struct Point {
  x: int
  y: int
  label: string?
}
```

### `InterfaceDecl`

```rust
InterfaceDecl { name: String, type_params: Vec<TypeParam>, methods: Vec<InterfaceMethod> }
```

An interface declaration listing required method signatures. Structs satisfy
an interface implicitly if their `impl` block provides all required methods.

```harn
interface Displayable {
  fn display(self) -> string
}
```

### `ImplBlock`

```rust
ImplBlock { type_name: String, methods: Vec<SNode> }
```

Attaches methods to a struct type. Each entry in `methods` is a `FnDecl`
whose first parameter is `self`.

```harn
impl Point {
  fn distance(self, other: Point) -> float {
    sqrt((self.x - other.x) ** 2 + (self.y - other.y) ** 2)
  }
}
```

## Control flow

### `IfElse`

```rust
IfElse { condition: Box<SNode>, then_body: Vec<SNode>, else_body: Option<Vec<SNode>> }
```

Conditional execution. An `else if` chain produces a nested `IfElse` inside
the `else_body`.

```harn
if x > 0 {
  positive()
} else {
  negative()
}
```

### `ForIn`

```rust
ForIn { pattern: BindingPattern, iterable: Box<SNode>, body: Vec<SNode> }
```

Iteration over a list, dict, string, or range.

```harn
for item in [1, 2, 3] {
  process(item)
}
```

### `MatchExpr`

```rust
MatchExpr { value: Box<SNode>, arms: Vec<MatchArm> }
```

Pattern matching. Each `MatchArm` has a `pattern`, optional `guard`, and `body`.

```harn
match status {
  "ok" -> handle_ok()
  "error" -> handle_error()
  _ -> handle_other()
}
```

### `WhileLoop`

```rust
WhileLoop { condition: Box<SNode>, body: Vec<SNode> }
```

Repeats the body while the condition is truthy.

```harn
while i < 10 {
  i = i + 1
}
```

### `Retry`

```rust
Retry { count: Box<SNode>, body: Vec<SNode> }
```

Executes the body up to `count` times, retrying on error.

```harn
retry 3 {
  attempt_fix()
}
```

### `GuardStmt`

```rust
GuardStmt { condition: Box<SNode>, else_body: Vec<SNode> }
```

Guard clause: if the condition is falsy, execute the else body (which
must diverge via `return`, `throw`, or `break`).

```harn
guard len(items) > 0 else {
  return nil
}
```

### `RequireStmt`

```rust
RequireStmt { condition: Box<SNode>, message: Option<Box<SNode>> }
```

Assertion that throws if the condition is falsy.

```harn
require x > 0, "x must be positive"
```

### `ReturnStmt`

```rust
ReturnStmt { value: Option<Box<SNode>> }
```

Returns from the current pipeline or function.

### `BreakStmt` / `ContinueStmt`

Terminal nodes for loop control flow.

### `ThrowStmt`

```rust
ThrowStmt { value: Box<SNode> }
```

Throws a value as an error.

```harn
throw {code: 404, msg: "not found"}
```

### `TryCatch`

```rust
TryCatch { body: Vec<SNode>, error_var: Option<String>, error_type: Option<TypeExpr>, catch_body: Vec<SNode>, finally_body: Option<Vec<SNode>> }
```

Error handling with optional typed catch and finally blocks.

```harn
try {
  risky_operation()
} catch (e: NetworkError) {
  log(e)
} finally {
  cleanup()
}
```

### `TryExpr`

```rust
TryExpr { body: Vec<SNode> }
```

A try-expression (no catch). Wraps the result as `Result.Ok(value)` or
`Result.Err(error)`.

```harn
let result = try { json_parse(raw_input) }
```

### `TryOperator`

```rust
TryOperator { operand: Box<SNode> }
```

Postfix `?` operator. Unwraps `Result.Ok(v)` to `v`, propagates
`Result.Err(e)` from the enclosing function.

```harn
let value = might_fail()?
```

## Concurrency

### `SpawnExpr`

```rust
SpawnExpr { body: Vec<SNode> }
```

Spawns an asynchronous task and returns a task handle.

```harn
let handle = spawn {
  long_running_work()
}
```

### `Parallel`

```rust
Parallel { mode: ParallelMode, expr: Box<SNode>, variable: Option<String>, body: Vec<SNode> }
```

Unified parallel execution node. The `mode` determines behavior:

- `ParallelMode::Count` — executes `body` concurrently `expr` times. Variable is bound to iteration index (0-based).
- `ParallelMode::Each` — maps over `expr` list concurrently. Variable is bound to each element.
- `ParallelMode::Settle` — like Each, but collects all results (including errors) instead of failing fast.

```harn
parallel 3 { i -> compute(i) }
parallel each items { item -> transform(item) }
parallel settle urls { url -> fetch(url) }
```

### `SelectExpr`

```rust
SelectExpr { cases: Vec<SelectCase>, timeout: Option<(Box<SNode>, Vec<SNode>)>, default_body: Option<Vec<SNode>> }
```

Waits on multiple channels. Each `SelectCase` has a `variable`, `channel`,
and `body`. Optional timeout and default branches.

```harn
select {
  msg <- inbox -> handle(msg)
  sig <- signals -> shutdown(sig)
  timeout 5s -> log("timed out")
}
```

### `DeadlineBlock`

```rust
DeadlineBlock { duration: Box<SNode>, body: Vec<SNode> }
```

Wraps a block with a deadline. If the body doesn't complete within the
duration, an error is thrown.

```harn
deadline 30s {
  slow_operation()
}
```

### `MutexBlock`

```rust
MutexBlock { body: Vec<SNode> }
```

Mutual exclusion block for concurrent access.

### `YieldExpr`

```rust
YieldExpr { value: Option<Box<SNode>> }
```

Yields control to the host, optionally with a value.

## Expressions

### `FunctionCall`

```rust
FunctionCall { name: String, args: Vec<SNode> }
```

Calls a function or builtin by name. Arguments may include `Spread` nodes.

```harn
read_file("config.json")
add(...args)
```

### `MethodCall`

```rust
MethodCall { object: Box<SNode>, method: String, args: Vec<SNode> }
```

Calls a method on an object.

```harn
list.map({ x -> x * 2 })
```

### `OptionalMethodCall`

```rust
OptionalMethodCall { object: Box<SNode>, method: String, args: Vec<SNode> }
```

Optional chaining method call. Returns nil if the object is nil.

```harn
result?.to_string()
```

### `PropertyAccess`

```rust
PropertyAccess { object: Box<SNode>, property: String }
```

Accesses a property on an object (dict field, struct field, `.count`, etc.).

```harn
result.name
```

### `OptionalPropertyAccess`

```rust
OptionalPropertyAccess { object: Box<SNode>, property: String }
```

Optional chaining property access. Returns nil if the object is nil.

```harn
user?.email
```

### `SubscriptAccess`

```rust
SubscriptAccess { object: Box<SNode>, index: Box<SNode> }
```

Accesses an element by index (list) or key (dict).

```harn
items[0]
config["key"]
```

### `SliceAccess`

```rust
SliceAccess { object: Box<SNode>, start: Option<Box<SNode>>, end: Option<Box<SNode>> }
```

Slice access with optional start and end bounds.

```harn
items[1:3]
text[:5]
```

### `BinaryOp`

```rust
BinaryOp { op: String, left: Box<SNode>, right: Box<SNode> }
```

Binary operation. Operators:
`+`, `-`, `*`, `/`, `%`, `**`, `==`, `!=`, `<`, `>`, `<=`, `>=`,
`&&`, `||`, `??`, `|>`, `in`, `not_in`.

### `UnaryOp`

```rust
UnaryOp { op: String, operand: Box<SNode> }
```

Unary prefix operation: `!` (logical not) or `-` (negation).

### `Ternary`

```rust
Ternary { condition: Box<SNode>, true_expr: Box<SNode>, false_expr: Box<SNode> }
```

Conditional expression.

```harn
x > 0 ? "positive" : "non-positive"
```

### `Assignment`

```rust
Assignment { target: Box<SNode>, value: Box<SNode>, op: Option<String> }
```

Assigns a value to a mutable variable. `op` is `None` for plain `=`,
or `Some("+")` for `+=`, etc.

### `RangeExpr`

```rust
RangeExpr { start: Box<SNode>, end: Box<SNode>, inclusive: bool }
```

Range expression. `inclusive: true` is the default (`a to b`); add a trailing `exclusive` modifier for the half-open form.

```harn
0 to 10 exclusive   // [0, 10)
0 to 9              // [0, 9] inclusive
```

### `DeferStmt`

```rust
DeferStmt { body: Vec<SNode> }
```

Defer statement — body runs at scope exit (on return or throw).

```harn
defer { cleanup() }
```

### `EnumConstruct`

```rust
EnumConstruct { enum_name: String, variant: String, args: Vec<SNode> }
```

Constructs an enum variant.

```harn
Color.Custom(255, 0, 0)
```

### `StructConstruct`

```rust
StructConstruct { struct_name: String, fields: Vec<DictEntry> }
```

Constructs a struct instance with named fields.

```harn
Point { x: 10, y: 20 }
```

## Literals

### `InterpolatedString`

```rust
InterpolatedString(Vec<StringSegment>)
```

A string with embedded expressions. Each `StringSegment` is
`Literal(String)` or `Expression(String, line, col)`.

```harn
"hello ${name}, result: ${x + 1}"
```

### `StringLiteral(String)` / `IntLiteral(i64)` / `FloatLiteral(f64)` / `BoolLiteral(bool)` / `NilLiteral`

Constant value terminals.

### `DurationLiteral(u64)`

Duration in milliseconds: `500ms`, `5s`, `30m`, `2h`.

### `Identifier(String)`

A variable or function name reference.

### `ListLiteral(Vec<SNode>)`

A list literal with zero or more element expressions.

```harn
[1, "two", true]
```

### `DictLiteral(Vec<DictEntry>)`

A dictionary literal. Each `DictEntry` has a `key` and `value` node.
Bare-identifier keys become `StringLiteral` during parsing. Computed keys
use bracket syntax: `{[expr]: value}`.

```harn
{name: "test", count: 42}
```

### `Spread(Box<SNode>)`

Spread expression `...expr` inside list/dict literals or function calls.

## Blocks

### `Block(Vec<SNode>)`

A sequence of statements in a child scope.

### `Closure`

```rust
Closure { params: Vec<TypedParam>, body: Vec<SNode>, fn_syntax: bool }
```

A closure literal. `fn_syntax: true` when written as `fn(params) { body }`.
Parameters may include type annotations.

```harn
{ x -> x * 2 }
fn(a: int, b: int) -> int { a + b }
```

## Supporting types

| Type | Fields | Used in |
|------|--------|---------|
| `SNode` | `Spanned<Node>` — node + source span | everywhere |
| `BindingPattern` | `Identifier(String)`, `Dict(Vec<...>)`, `List(Vec<...>)` | let/var/for |
| `TypeExpr` | `Named`, `List`, `Optional`, `Union`, `Shape`, `FnType`, `Generic` | type annotations |
| `TypeParam` | `name: String`, `constraint: Option<String>` | generics |
| `TypedParam` | `name`, `type_expr`, `default_value`, `is_rest` | fn params |
| `WhereClause` | `type_name: String`, `bound: String` | generic constraints |
| `MatchArm` | `pattern: SNode`, `guard: Option<SNode>`, `body: Vec<SNode>` | match |
| `SelectCase` | `variable`, `channel`, `body` | select |
| `DictEntry` | `key: SNode`, `value: SNode` | dict/struct |
| `EnumVariant` | `name: String`, `fields: Vec<TypedParam>` | enum decl |
| `StructField` | `name`, `type_expr`, `optional` | struct decl |
| `InterfaceMethod` | `name`, `type_params`, `params`, `return_type` | interface decl |
| `StringSegment` | `Literal(String)`, `Expression(String, usize, usize)` | interpolation |
