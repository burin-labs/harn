# Language basics

This guide covers the core syntax and semantics of Harn.

## Pipelines

Pipelines are the top-level organizational unit. A Harn program is one or more
pipelines. The runtime executes the pipeline named `default`, or the first
one declared.

```javascript
pipeline default(task) {
  log("Hello from the default pipeline")
}

pipeline other(task) {
  log("This only runs if called or if there's no default")
}
```

Pipeline parameters `task` and `project` are injected by the host runtime.
A `context` dict with keys `task`, `project_root`, and `task_type` is
always available.

## Variables

`let` creates immutable bindings. `var` creates mutable ones.

```javascript
let name = "Alice"
var counter = 0

counter = counter + 1  // ok
name = "Bob"           // error: immutable assignment
```

## Types and values

Harn is dynamically typed with optional type annotations.

| Type | Example | Notes |
|---|---|---|
| `int` | `42` | Platform-width integer |
| `float` | `3.14` | Double-precision |
| `string` | `"hello"` | UTF-8, supports interpolation |
| `bool` | `true`, `false` | |
| `nil` | `nil` | Null value |
| `list` | `[1, 2, 3]` | Heterogeneous, ordered |
| `dict` | `{name: "Alice"}` | String-keyed map |
| `closure` | `{ x -> x + 1 }` | First-class function |
| `duration` | `5s`, `100ms` | Time duration |

### Type annotations

Annotations are optional and checked at runtime:

```javascript
let x: int = 42
let name: string = "hello"
let nums: list[int] = [1, 2, 3]

fn add(a: int, b: int) -> int {
  return a + b
}
```

Supported type expressions: `int`, `float`, `string`, `bool`, `nil`, `list`,
`list[T]`, `dict`, union types (`string | nil`), and shape types
(`{x: int, y: int}`).

### Truthiness

These values are falsy: `false`, `nil`, `0`, `0.0`, `""`, `[]`, `{}`. Everything else is truthy.

## Strings

### Interpolation

```javascript
let name = "world"
log("Hello, ${name}!")
log("2 + 2 = ${2 + 2}")
```

Any expression works inside `${}`.

### Multi-line strings

```javascript
let doc = """
  This is a multi-line string.
  Common leading whitespace is stripped.
  Interpolation is NOT supported here.
"""
```

### Escape sequences

`\n` (newline), `\t` (tab), `\\` (backslash), `\"` (quote), `\$` (dollar sign).

### String methods

```javascript
"hello".count                    // 5
"hello".empty                    // false
"hello".contains("ell")          // true
"hello".replace("l", "r")       // "herro"
"a,b,c".split(",")              // ["a", "b", "c"]
"  hello  ".trim()              // "hello"
"hello".starts_with("he")       // true
"hello".ends_with("lo")         // true
"hello".uppercase()             // "HELLO"
"hello".lowercase()             // "hello"
"hello world".substring(0, 5)   // "hello"
```

## Operators

Ordered by precedence (lowest to highest):

| Precedence | Operators | Description |
|---|---|---|
| 1 | `\|>` | Pipe |
| 2 | `? :` | Ternary conditional |
| 3 | `??` | Nil coalescing |
| 4 | `\|\|` | Logical OR (short-circuit) |
| 5 | `&&` | Logical AND (short-circuit) |
| 6 | `==` `!=` | Equality |
| 7 | `<` `>` `<=` `>=` | Comparison |
| 8 | `+` `-` | Add, subtract, string/list concat |
| 9 | `*` `/` | Multiply, divide |
| 10 | `!` `-` | Unary not, negate |
| 11 | `.` `[]` `()` | Member access, subscript, call |

Division by zero returns `nil`. Integer division truncates.

## Control flow

### if/else

```javascript
if score > 90 {
  log("A")
} else if score > 80 {
  log("B")
} else {
  log("C")
}
```

Can be used as an expression: `let grade = if score > 90 { "A" } else { "B" }`

### for/in

```javascript
for item in [1, 2, 3] {
  log(item)
}

// Dict iteration yields {key, value} entries sorted by key
for entry in {a: 1, b: 2} {
  log("${entry.key}: ${entry.value}")
}
```

### while

```javascript
var i = 0
while i < 10 {
  log(i)
  i = i + 1
}
```

Safety limit of 10,000 iterations.

### match

```javascript
match status {
  "active" -> { log("Running") }
  "stopped" -> { log("Halted") }
}
```

Patterns are expressions compared by equality. First match wins. No match returns `nil`.

### guard

Early exit if a condition isn't met:

```javascript
guard x > 0 else {
  return "invalid"
}
// x is guaranteed > 0 here
```

### Ranges

```javascript
for i in 1 thru 5 {   // inclusive: 1, 2, 3, 4, 5
  log(i)
}

for i in 0 upto 3 {   // exclusive: 0, 1, 2
  log(i)
}
```

## Functions and closures

### Named functions

```javascript
fn double(x) {
  return x * 2
}

fn greet(name: string) -> string {
  return "Hello, ${name}!"
}
```

Functions can be declared at the top level (for library files) or inside pipelines.

### Closures

```javascript
let square = { x -> x * x }
let add = { a, b -> a + b }

log(square(4))     // 16
log(add(2, 3))     // 5
```

Closures capture their lexical environment at definition time. Parameters are immutable.

### Higher-order functions

```javascript
let nums = [1, 2, 3, 4, 5]

nums.map({ x -> x * 2 })           // [2, 4, 6, 8, 10]
nums.filter({ x -> x > 3 })        // [4, 5]
nums.reduce(0, { acc, x -> acc + x }) // 15
nums.find({ x -> x == 3 })         // 3
nums.any({ x -> x > 4 })           // true
nums.all({ x -> x > 0 })           // true
nums.flat_map({ x -> [x, x] })     // [1, 1, 2, 2, 3, 3, 4, 4, 5, 5]
```

## Pipe operator

The pipe operator `|>` passes the left side as the argument to the right side:

```javascript
let result = data
  |> { list -> list.filter({ x -> x > 0 }) }
  |> { list -> list.map({ x -> x * 2 }) }
  |> json_stringify
```

## Collections

### Lists

```javascript
let nums = [1, 2, 3]
nums.count          // 3
nums.first          // 1
nums.last           // 3
nums.empty          // false
nums[0]             // 1 (subscript access)
```

Lists support `+` for concatenation: `[1, 2] + [3, 4]` yields `[1, 2, 3, 4]`.

### Dicts

```javascript
let user = {name: "Alice", age: 30}
user.name           // "Alice" (property access)
user["age"]         // 30 (subscript access)
user.missing        // nil (missing keys return nil)
user.has("email")   // false

user.keys()         // ["age", "name"] (sorted)
user.values()       // [30, "Alice"]
user.entries()      // [{key: "age", value: 30}, ...]
user.merge({role: "admin"})  // new dict with merged keys
user.map_values({ v -> to_string(v) })
user.filter({ v -> type_of(v) == "int" })
```

Computed keys use bracket syntax: `{[dynamic_key]: value}`.

Quoted string keys are also supported for JSON compatibility:
`{"content-type": "json"}`. The formatter normalizes simple quoted keys
to unquoted form and non-identifier keys to computed key syntax.

Keywords can be used as dict keys and property names: `{type: "read"}`,
`op.type`.

Dicts iterate in **sorted key order** (alphabetical). This means
`for k in dict` is deterministic and reproducible, but does not preserve
insertion order.

## Enums and structs

### Enums

```javascript
enum Status {
  Active
  Inactive
  Pending(reason)
  Failed(code, message)
}

let s = Status.Pending("waiting")
match s.variant {
  "Pending" -> { log(s.fields[0]) }
  "Active" -> { log("ok") }
}
```

### Structs

```javascript
struct Point {
  x: int
  y: int
}

let p = {x: 10, y: 20}
log(p.x)
```

## Duration literals

```javascript
let d1 = 500ms   // 500 milliseconds
let d2 = 5s      // 5 seconds
let d3 = 2m      // 2 minutes
let d4 = 1h      // 1 hour
```

Durations can be passed to `sleep()` and used in `deadline` blocks.

## Comments

```javascript
// Line comment

/* Block comment
   /* Nested block comments are supported */
   Still inside the outer comment */
```
