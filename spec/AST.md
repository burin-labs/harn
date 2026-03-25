# Harn AST node catalog

All AST nodes are cases of the `HarnNode` enum, defined in `Sources/BurinCore/Harn/HarnNode.swift`. The enum is `indirect` (nodes can contain other nodes) and `Equatable`.

## Declarations

### `pipeline`

```
pipeline(name: String, params: [String], body: [HarnNode], extends: String?)
```

A named pipeline declaration. `params` are the formal parameter names. `body` is the list of statements. `extends` is the optional parent pipeline name.

```harn
pipeline default(task, project) {
  let x = 1
}
```

### `letBinding`

```
letBinding(name: String, value: HarnNode)
```

An immutable variable binding.

```harn
let result = compute()
```

### `varBinding`

```
varBinding(name: String, value: HarnNode)
```

A mutable variable binding.

```harn
var count = 0
```

### `overrideDecl`

```
overrideDecl(name: String, params: [String], body: [HarnNode])
```

An override declaration inside a child pipeline that extends a parent.

```harn
pipeline child(task) extends parent {
  override fill_strategy(target) {
    custom_fill(target)
  }
}
```

### `importDecl`

```
importDecl(path: String)
```

Imports another `.harn` file by path.

```harn
import "shared/common.harn"
```

## Control flow

### `ifElse`

```
ifElse(condition: HarnNode, then: [HarnNode], elseBlock: [HarnNode]?)
```

Conditional execution. `elseBlock` is `nil` when there is no `else` branch. An `else if` chain produces a nested `ifElse` inside the `elseBlock` array.

```harn
if x > 0 {
  positive()
} else {
  negative()
}
```

### `forIn`

```
forIn(variable: String, iterable: HarnNode, body: [HarnNode])
```

Iteration over a list or dict.

```harn
for item in [1, 2, 3] {
  process(item)
}
```

### `matchExpr`

```
matchExpr(value: HarnNode, arms: [(pattern: HarnNode, body: [HarnNode])])
```

Pattern matching. Each arm has a pattern expression and a body. The first arm whose pattern equals the match value executes.

```harn
match status {
  "ok" -> { handle_ok() }
  "error" -> { handle_error() }
}
```

### `whileLoop`

```
whileLoop(condition: HarnNode, body: [HarnNode])
```

Repeats the body while the condition is truthy.

```harn
while i < 10 {
  i = i + 1
}
```

### `retry`

```
retry(count: HarnNode, body: [HarnNode])
```

Executes the body up to `count` times, retrying on error.

```harn
retry 3 {
  attempt_fix()
}
```

### `returnStmt`

```
returnStmt(HarnNode?)
```

Returns from the current pipeline or function. The value is optional.

```harn
return result
```

### `tryCatch`

```
tryCatch(body: [HarnNode], errorVar: String?, catchBody: [HarnNode])
```

Error handling. `errorVar` is the optional name bound to the caught error in the catch block.

```harn
try {
  risky_operation()
} catch (e) {
  log(e)
}
```

### `fnDecl`

```
fnDecl(name: String, params: [String], body: [HarnNode])
```

Named function declaration. Creates a closure value and binds it in the current scope.

```harn
fn add(a, b) {
  return a + b
}
```

### `spawnExpr`

```
spawnExpr(body: [HarnNode])
```

Spawns an asynchronous task and returns a task handle.

```harn
let handle = spawn {
  long_running_work()
}
```

## Concurrency

### `parallel`

```
parallel(count: HarnNode, variable: String?, body: [HarnNode])
```

Executes `body` concurrently `count` times. The optional `variable` is bound to the iteration index (0-based).

```harn
parallel(3) { i ->
  compute(i)
}
```

### `parallelMap`

```
parallelMap(list: HarnNode, variable: String, body: [HarnNode])
```

Maps over a list concurrently. Each element is bound to `variable`.

```harn
parallel_map(items) { item ->
  transform(item)
}
```

## Expressions

### `functionCall`

```
functionCall(name: String, args: [HarnNode])
```

Calls a function or builtin by name.

```harn
read_file("config.json")
```

### `methodCall`

```
methodCall(object: HarnNode, method: String, args: [HarnNode])
```

Calls a method on an object.

```harn
list.map({ x -> x * 2 })
```

### `propertyAccess`

```
propertyAccess(object: HarnNode, property: String)
```

Accesses a property on an object (dict field, list `.count`, etc.).

```harn
result.name
```

### `subscriptAccess`

```
subscriptAccess(object: HarnNode, index: HarnNode)
```

Accesses an element by index (list) or key (dict).

```harn
items[0]
config["key"]
```

### `binaryOp`

```
binaryOp(op: String, left: HarnNode, right: HarnNode)
```

A binary operation. `op` is the operator string: `+`, `-`, `*`, `/`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `||`, `??`, `|>`.

```harn
1 + 2
x == y
a |> transform
```

### `unaryOp`

```
unaryOp(op: String, operand: HarnNode)
```

A unary prefix operation. `op` is `!` (logical not) or `-` (negation).

```harn
!done
-5
```

### `ternary`

```
ternary(condition: HarnNode, trueExpr: HarnNode, falseExpr: HarnNode)
```

Conditional expression.

```harn
x > 0 ? "positive" : "non-positive"
```

### `assignment`

```
assignment(target: HarnNode, value: HarnNode)
```

Assigns a new value to an existing mutable variable. `target` is always an `identifier` node.

```harn
count = count + 1
```

### `throwStmt`

```
throwStmt(HarnNode)
```

Throws a value as an error.

```harn
throw "something went wrong"
throw {code: 404, msg: "not found"}
```

## Literals

### `interpolatedString`

```
interpolatedString([StringSegment])
```

A string with embedded expressions. Each `StringSegment` is either `.literal(String)` or `.expression(String)`.

```harn
"hello ${name}, result: ${x + 1}"
```

### `stringLiteral`

```
stringLiteral(String)
```

A plain string constant.

```harn
"hello world"
```

### `intLiteral`

```
intLiteral(Int)
```

An integer constant.

```harn
42
```

### `floatLiteral`

```
floatLiteral(Double)
```

A floating-point constant.

```harn
3.14
```

### `boolLiteral`

```
boolLiteral(Bool)
```

A boolean constant.

```harn
true
false
```

### `nilLiteral`

```
nilLiteral
```

The nil value.

```harn
nil
```

### `identifier`

```
identifier(String)
```

A variable or function name reference.

```harn
count
my_variable
```

### `listLiteral`

```
listLiteral([HarnNode])
```

A list literal with zero or more element expressions.

```harn
[1, "two", true]
```

### `dictLiteral`

```
dictLiteral([(key: HarnNode, value: HarnNode)])
```

A dictionary literal with key-value pairs. Bare-identifier keys are converted to `stringLiteral` nodes during parsing. Computed keys use bracket syntax.

```harn
{name: "test", count: 42}
{[dynamic_key]: value}
```

## Blocks

### `block`

```
block([HarnNode])
```

A sequence of statements evaluated in a child scope. Not directly produced by the parser but used internally.

### `closure`

```
closure(params: [String], body: [HarnNode])
```

A closure literal with parameter names and a body.

```harn
{ x -> x * 2 }
{ a, b -> a + b }
```
