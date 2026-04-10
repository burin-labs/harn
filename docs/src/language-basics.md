# Language basics

This guide covers the core syntax and semantics of Harn.

## Implicit pipeline

Harn files can contain top-level code without a `pipeline` block. The
runtime wraps it in an implicit pipeline automatically:

```harn
let x = 1 + 2
println(x)

fn double(n) {
  return n * 2
}
println(double(5))
```

This is convenient for scripts, experiments, and small programs.

## Pipelines

For larger programs, organize code into named pipelines. The runtime
executes the pipeline named `default`, or the first one declared.

```harn
pipeline default(task) {
  println("Hello from the default pipeline")
}

pipeline other(task) {
  println("This only runs if called or if there's no default")
}
```

Pipeline parameters `task` and `project` are injected by the host runtime.
A `context` dict with keys `task`, `project_root`, and `task_type` is
always available.

## Variables

`let` creates immutable bindings. `var` creates mutable ones.

```harn
let name = "Alice"
var counter = 0

counter = counter + 1  // ok
name = "Bob"           // error: immutable assignment
```

Bindings are lexically scoped. Each `if` branch, loop body, `catch` body, and
explicit `{ ... }` block gets its own scope, so inner bindings can shadow outer
names without colliding:

```harn
let status = "outer"

if true {
  let status = "inner"
  println(status)  // inner
}

println(status)    // outer
```

If you want to update an outer binding from inside a block, declare it with
`var` outside the block and assign to it inside the branch or loop body.

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

Annotations are optional and checked at compile time:

```harn
let x: int = 42
let name: string = "hello"
let nums: list<int> = [1, 2, 3]

fn add(a: int, b: int) -> int {
  return a + b
}
```

Supported type expressions: `int`, `float`, `string`, `bool`, `nil`, `list`,
`list<T>`, `dict`, `dict<K, V>`, union types (`string | nil`), and structural
shape types (`{name: string, age: int}`).

Parameter type annotations for primitive types (`int`, `float`, `string`,
`bool`, `list`, `dict`, `set`, `nil`, `closure`) are enforced at runtime.
Calling a function with the wrong type produces a `TypeError`:

```harn,ignore
fn add(a: int, b: int) -> int {
  return a + b
}

add("hello", "world")
// TypeError: parameter 'a' expected int, got string (hello)
```

### Structural types (shapes)

Shape types describe the expected fields of a dict. The type checker verifies
that required fields are present with compatible types. Extra fields are allowed
(width subtyping).

```harn
let user: {name: string, age: int} = {name: "Alice", age: 30}
let config: {host: string, port?: int} = {host: "localhost"}

fn greet(u: {name: string}) -> string {
  return "hi ${u["name"]}"
}
greet({name: "Bob", age: 25})
```

Use `type` aliases for reusable shape definitions:

```harn
type Config = {model: string, max_tokens: int}
let cfg: Config = {model: "gpt-4", max_tokens: 100}
```

### Truthiness

These values are falsy: `false`, `nil`, `0`, `0.0`, `""`, `[]`, `{}`. Everything else is truthy.

## Strings

### Interpolation

```harn
let name = "world"
println("Hello, ${name}!")
println("2 + 2 = ${2 + 2}")
```

Any expression works inside `${}`.

### Raw strings

Raw strings use the `r"..."` prefix. No escape processing or interpolation
is performed -- backslashes and dollar signs are taken literally. Useful for
regex patterns and file paths:

```harn
let pattern = r"\d+\.\d+"
let path = r"C:\Users\alice\docs"
```

Raw strings cannot span multiple lines.

### Multi-line strings

```harn
let doc = """
  This is a multi-line string.
  Common leading whitespace is stripped.
"""
```

Multi-line strings support `${expression}` interpolation with automatic
indent stripping:

```harn
let name = "world"
let greeting = """
  Hello, ${name}!
  Welcome to Harn.
"""
```

### Escape sequences

`\n` (newline), `\t` (tab), `\\` (backslash), `\"` (quote), `\$` (dollar sign).

### String methods

```harn
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
| 7 | `<` `>` `<=` `>=` `in` `not in` | Comparison, membership |
| 8 | `+` `-` | Add, subtract, string/list concat |
| 9 | `*` `/` | Multiply, divide |
| 10 | `!` `-` | Unary not, negate |
| 11 | `.` `?.` `[]` `[:]` `()` `?` | Member access, optional chaining, subscript, slice, call, try |

Division by zero returns `nil`. Integer division truncates.
Arithmetic operators are strictly typed — mismatched operands (e.g.
`"hello" + 5`) produce a `TypeError`. Use `to_string()` or string
interpolation (`"value=${x}"`) for explicit conversion.

### Optional chaining (`?.`)

Access properties or call methods on values that might be nil. Returns
nil instead of erroring when the receiver is nil:

```harn
let user = nil
println(user?.name)           // nil (no error)
println(user?.greet("hi"))    // nil (method not called)

let d = {name: "Alice"}
println(d?.name)              // Alice
```

Chains propagate nil: `a?.b?.c` returns nil if any step is nil.

### List and string slicing (`[start:end]`)

Extract sublists or substrings using slice syntax:

```harn
let items = [10, 20, 30, 40, 50]
println(items[1:3])   // [20, 30]
println(items[:2])    // [10, 20]
println(items[3:])    // [40, 50]
println(items[-2:])   // [40, 50]

let s = "hello world"
println(s[0:5])       // hello
println(s[-5:])       // world
```

Negative indices count from the end. Omit start for 0, omit end for
length.

### Try operator (`?`)

The postfix `?` operator works with `Result` values (`Ok` / `Err`). It
unwraps `Ok` values and propagates `Err` values by returning early from
the enclosing function:

```harn
fn divide(a, b) {
  if b == 0 {
    return Err("division by zero")
  }
  return Ok(a / b)
}

fn compute(x) {
  let result = divide(x, 2)?   // unwraps Ok, or returns Err early
  return Ok(result + 10)
}

fn compute_zero(x) {
  let result = divide(x, 0)?   // divide returns Err, ? propagates it
  return Ok(result + 10)
}

println(compute(20))       // Result.Ok(20)
println(compute_zero(20))  // Result.Err(division by zero)
```

Multiple `?` calls can be chained in a single function to build
pipelines that short-circuit on the first error.

### Membership operators (`in`, `not in`)

Test whether a value is contained in a collection:

```harn
// Lists
println(3 in [1, 2, 3])          // true
println(6 not in [1, 2, 3])      // true

// Strings (substring containment)
println("world" in "hello world") // true
println("xyz" not in "hello")     // true

// Dicts (key membership)
let data = {name: "Alice", age: 30}
println("name" in data)           // true
println("email" not in data)      // true

// Sets
let s = set(1, 2, 3)
println(2 in s)                   // true
println(5 not in s)               // true
```

## Control flow

### if/else

```harn
if score > 90 {
  println("A")
} else if score > 80 {
  println("B")
} else {
  println("C")
}
```

Can be used as an expression: `let grade = if score > 90 { "A" } else { "B" }`

### for/in

```harn
for item in [1, 2, 3] {
  println(item)
}

// Dict iteration yields {key, value} entries sorted by key
for entry in {a: 1, b: 2} {
  println("${entry.key}: ${entry.value}")
}
```

### while

```harn
var i = 0
while i < 10 {
  println(i)
  i = i + 1
}
```

Safety limit of 10,000 iterations.

### match

```harn
match status {
  "active" -> { println("Running") }
  "stopped" -> { println("Halted") }
}
```

Patterns are expressions compared by equality. First match wins. No match returns `nil`.

### guard

Early exit if a condition isn't met:

```harn
guard x > 0 else {
  return "invalid"
}
// x is guaranteed > 0 here
```

### Ranges

```harn
for i in 1 thru 5 {   // inclusive: 1, 2, 3, 4, 5
  println(i)
}

for i in 0 upto 3 {   // exclusive: 0, 1, 2
  println(i)
}
```

## Functions and closures

### Named functions

```harn
fn double(x) {
  return x * 2
}

fn greet(name: string) -> string {
  return "Hello, ${name}!"
}
```

Functions can be declared at the top level (for library files) or inside pipelines.

### Rest parameters

Use `...name` as the last parameter to collect any remaining arguments into
a list:

```harn
fn sum(...nums) {
  var total = 0
  for n in nums {
    total = total + n
  }
  return total
}
println(sum(1, 2, 3))  // 6

fn log(level, ...parts) {
  println("[${level}] ${join(parts, " ")}")
}
log("INFO", "server", "started")  // [INFO] server started
```

If no extra arguments are provided, the rest parameter is an empty list.

### Closures

```harn
let square = { x -> x * x }
let add = { a, b -> a + b }

println(square(4))     // 16
println(add(2, 3))     // 5
```

Closures capture their lexical environment at definition time. Parameters are immutable.

### Higher-order functions

```harn
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

```harn
let result = data
  |> { list -> list.filter({ x -> x > 0 }) }
  |> { list -> list.map({ x -> x * 2 }) }
  |> json_stringify
```

### Pipe placeholder (`_`)

Use `_` to control where the piped value is placed in the call:

```harn
"hello world" |> split(_, " ")       // ["hello", "world"]
[3, 1, 2] |> _.sort()               // [1, 2, 3]
items |> len(_)                      // length of items
"world" |> replace("hello _", "_", _) // "hello world"
```

Without `_`, the value is passed as the sole argument to a closure or
function name.

## Multiline expressions

Binary operators, method chains, and pipes can span multiple lines:

```harn
let message = "hello"
  + " "
  + "world"

let result = items
  .filter({ x -> x > 0 })
  .map({ x -> x * 2 })

let valid = check_a()
  && check_b()
  || fallback()
```

Note: `-` does not continue across lines because it doubles as unary
negation.

A backslash at the end of a line forces the next line to continue the
current expression, even when no operator is present:

```harn
let long_value = some_function( \
  arg1, arg2, arg3 \
)
```

## Destructuring

Destructuring extracts values from dicts and lists into local variables.

### Dict destructuring

```harn
let person = {name: "Alice", age: 30}
let {name, age} = person
println(name)  // "Alice"
println(age)   // 30
```

### List destructuring

```harn
let items = [1, 2, 3, 4, 5]
let [first, ...rest] = items
println(first)  // 1
println(rest)   // [2, 3, 4, 5]
```

### Renaming

Use `:` to bind a dict field to a different variable name:

```harn
let data = {name: "Alice"}
let {name: user_name} = data
println(user_name)  // "Alice"
```

### Destructuring in for-in loops

```harn
let entries = [{key: "a", value: 1}, {key: "b", value: 2}]
for {key, value} in entries {
  println("${key}: ${value}")
}
```

### Default values

Pattern fields can specify defaults with `= expr`. The default is used when
the value would otherwise be `nil`:

```harn
let { name = "anon", role = "user" } = { name: "Alice" }
println(name)  // Alice
println(role)  // user

let [a = 0, b = 0, c = 0] = [1, 2]
println(c)     // 0

// Combine with renaming
let { name: display = "Unknown" } = {}
println(display)  // Unknown
```

### Missing keys and empty rest

Missing keys destructure to `nil` (unless a default is specified). A rest
pattern with no remaining items gives an empty collection:

```harn
let {name, email} = {name: "Alice"}
println(email)  // nil

let [only, ...rest] = [42]
println(rest)   // []
```

## Collections

### Lists

```harn
let nums = [1, 2, 3]
nums.count          // 3
nums.first          // 1
nums.last           // 3
nums.empty          // false
nums[0]             // 1 (subscript access)
```

Lists support `+` for concatenation: `[1, 2] + [3, 4]` yields `[1, 2, 3, 4]`.
Assigning to an out-of-bounds index throws an error.

### Dicts

```harn
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

### Sets

Sets are unordered collections of unique values. Duplicates are
automatically removed.

```harn
let s = set(1, 2, 3)          // create from individual values
let s2 = set([4, 5, 5, 6])   // create from a list (deduplicates)
let tags = set("a", "b", "c") // works with any value type
```

Set operations are provided as builtin functions:

```harn
let a = set(1, 2, 3)
let b = set(3, 4, 5)

set_contains(a, 2)       // true
set_contains(a, 99)      // false

set_union(a, b)          // set(1, 2, 3, 4, 5)
set_intersect(a, b)      // set(3)
set_difference(a, b)     // set(1, 2) -- items in a but not in b

set_add(a, 4)            // set(1, 2, 3, 4)
set_remove(a, 2)         // set(1, 3)
```

Sets support iteration with `for..in`:

```harn
var sum = 0
for item in set(10, 20, 30) {
  sum = sum + item
}
println(sum)  // 60
```

Convert a set to a list with `to_list()`:

```harn
let items = to_list(set(10, 20))
type_of(items)  // "list"
```

## Enums and structs

### Enums

```harn
enum Status {
  Active
  Inactive
  Pending(reason)
  Failed(code, message)
}

let s = Status.Pending("waiting")
match s.variant {
  "Pending" -> { println(s.fields[0]) }
  "Active" -> { println("ok") }
}
```

### Structs

```harn
struct Point {
  x: int
  y: int
}

let p = {x: 10, y: 20}
println(p.x)
```

Structs can also be constructed with the struct name as a constructor,
using named fields directly:

```harn
let p = Point { x: 10, y: 20 }
println(p.x)  // 10
```

Structs can declare type parameters when fields should stay connected:

```harn
struct Pair<A, B> {
  first: A
  second: B
}

let pair: Pair<int, string> = Pair { first: 1, second: "two" }
println(pair.second)  // two
```

### Impl blocks

Add methods to a struct with `impl`:

```harn
struct Point {
  x: int
  y: int
}

impl Point {
  fn distance(self) {
    return sqrt(self.x * self.x + self.y * self.y)
  }
  fn translate(self, dx, dy) {
    return Point { x: self.x + dx, y: self.y + dy }
  }
}

let p = Point { x: 3, y: 4 }
println(p.distance())       // 5.0
println(p.translate(10, 20)) // Point({x: 13, y: 24})
```

The first parameter must be `self`, which receives the struct instance.
Methods are called with dot syntax on values constructed with the struct
constructor.

## Interfaces

Interfaces let you define a contract: a set of methods that a type must
have. Harn uses **implicit satisfaction**, just like Go. A struct satisfies
an interface automatically if its `impl` block has all the required methods.
You never write `implements` or `impl Interface for Type`.

### Step 1: Define an interface

An interface lists method signatures without bodies:

```harn
interface Displayable {
  fn display(self) -> string
}
```

This says: any type that has a `display(self) -> string` method counts as
`Displayable`.

Interfaces can also be generic, and individual interface methods may declare
their own type parameters when the contract needs them:

```harn
interface Repository<T> {
  fn get(id: string) -> T
  fn map<U>(value: T, f: fn(T) -> U) -> U
}
```

Interfaces may also declare associated types when the contract needs to name
an implementation-defined type without making the whole interface generic:

```harn
interface Collection {
  type Item
  fn get(self, index: int) -> Item
}
```

### Step 2: Create structs with matching methods

```harn
struct Dog {
  name: string
  breed: string
}

impl Dog {
  fn display(self) -> string {
    return "${self.name} the ${self.breed}"
  }
}

struct Cat {
  name: string
  indoor: bool
}

impl Cat {
  fn display(self) -> string {
    let status = if self.indoor { "indoor" } else { "outdoor" }
    return "${self.name} (${status} cat)"
  }
}
```

Both `Dog` and `Cat` have a `display(self) -> string` method, so they
both satisfy `Displayable`. No extra annotation is needed.

### Step 3: Use the interface as a type

Now you can write a function that accepts any `Displayable`:

```harn
fn introduce(animal: Displayable) {
  println("Meet: ${animal.display()}")
}

let d = Dog({name: "Rex", breed: "Labrador"})
let c = Cat({name: "Whiskers", indoor: true})

introduce(d)  // Meet: Rex the Labrador
introduce(c)  // Meet: Whiskers (indoor cat)
```

The type checker verifies at compile time that `Dog` and `Cat` satisfy
`Displayable`. If a struct is missing a required method, you get a
clear error at the call site.

### Interfaces with multiple methods

Interfaces can require more than one method:

```harn
interface Serializable {
  fn serialize(self) -> string
  fn byte_size(self) -> int
}
```

### `guard`, `require`, and `assert`

These three forms serve different jobs:

- `guard condition else { ... }` handles expected control flow and narrows types after the guard.
- `require condition, "message"` enforces runtime invariants in normal code and throws on failure.
- `assert`, `assert_eq`, and `assert_ne` are for test pipelines. The linter
  warns when you use them in non-test code, and it nudges test pipelines away
  from `require`.

```harn
guard user != nil else {
  return "missing user"
}

require len(user.name) > 0, "user name cannot be empty"
```

A struct must implement all listed methods to satisfy the interface.

### Generic constraints

You can also use interfaces as constraints on generic type parameters:

```harn
fn log_item<T>(item: T) where T: Displayable {
  println("[LOG] ${item.display()}")
}
```

The `where T: Displayable` clause tells the type checker to verify that
whatever concrete type is passed for `T` satisfies `Displayable`. If it
does not, a compile-time error is produced. Generic parameters must also bind
consistently across arguments, so `fn<T>(a: T, b: T)` cannot be called with
mixed concrete types such as `(int, string)`. Container bindings like
`list<T>` preserve and validate their element type at call sites too.

## Spread in function calls

The spread operator `...` expands a list into individual function
arguments:

```harn
fn add(a, b, c) {
  return a + b + c
}

let nums = [1, 2, 3]
println(add(...nums))  // 6
```

You can mix regular arguments and spread arguments:

```harn
let rest = [2, 3]
println(add(1, ...rest))  // 6
```

Spread works in method calls too:

```harn
let point = Point({x: 0, y: 0})
let deltas = [10, 20]
let moved = point.translate(...deltas)
```

## Try-expression

The `try` keyword without a `catch` block is a try-expression. It
evaluates its body and wraps the outcome in a `Result`:

```harn
let result = try { json_parse(raw_input) }
// Result.Ok(parsed_data)  -- if parsing succeeds
// Result.Err("invalid JSON: ...") -- if parsing throws
```

This is the complement of the `?` operator. Use `try` to enter
Result-land (catching errors into `Result.Err`), and `?` to exit
Result-land (propagating errors upward):

```harn
fn safe_divide(a, b) {
  return try { a / b }
}

fn compute(x) {
  let half = safe_divide(x, 2)?  // unwrap Ok or propagate Err
  return Ok(half + 10)
}
```

No `catch` or `finally` is needed. If a `catch` follows `try`, it is
parsed as the traditional `try`/`catch` statement instead.

## Ask expression

The `ask` expression is syntactic sugar for making an LLM call. It takes
a set of key-value fields and returns the LLM response as a string:

```harn
let answer = ask {
  system: "You are a helpful assistant.",
  user: "What is 2 + 2?"
}
println(answer)
```

Common fields include `system` (system prompt), `user` (user message),
`model`, `max_tokens`, and `provider`. The `ask` expression is equivalent
to building a dict and passing it to `llm_call`.

## Duration literals

```harn
let d1 = 500ms   // 500 milliseconds
let d2 = 5s      // 5 seconds
let d3 = 2m      // 2 minutes
let d4 = 1h      // 1 hour
```

Durations can be passed to `sleep()` and used in `deadline` blocks.

## Math constants

`pi` and `e` are global constants (not functions):

```harn
println(pi)    // 3.141592653589793
println(e)     // 2.718281828459045

let area = pi * r * r
```

## Named format placeholders

The `format` builtin supports both positional `{}` placeholders and named
`{key}` placeholders when the second argument is a dict:

```harn
// Positional
println(format("Hello, {}!", "world"))

// Named
println(format("Hello {name}, you are {age}.", {name: "Alice", age: 30}))
```

For simple cases, string interpolation with `${}` is usually more
convenient:

```harn
let name = "Alice"
println("Hello, ${name}!")
```

## Comments

```harn
// Line comment

/// HarnDoc comment for a public API
/// Use contiguous `///` lines directly above `pub fn`
pub fn greet(name: string) -> string {
  return "Hello, ${name}"
}

pub pipeline deploy(task) {
  return
}

pub enum Result {
  Ok(value: string)
  Err(message: string)
}

pub struct Config {
  host: string
  port?: int
}

/* Block comment
   /* Nested block comments are supported */
   Still inside the outer comment */
```
