<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Destructuring patterns

Destructuring binds multiple variables from a dict or list in a single
`let`, `var`, or `for`-`in` statement.

### Dict destructuring

```harn
const {name, age} = {name: "Alice", age: 30}
// name == "Alice", age == 30
```

Each field name in the pattern extracts the value for the matching key.
If the key is missing from the dict, the variable is bound to `nil`.
Use `_` as a discard binding when you want to ignore an extracted field:

```harn
const {name, debug: _} = {name: "Alice", debug: true}
// name == "Alice"; `_` is not bound
```

### Default values

Pattern fields can specify default values with `= expr` syntax. The
default expression is evaluated when the extracted value is `nil` (i.e.
when the key is missing from the dict or the index is out of bounds for
a list):

```harn
const { name = "workflow", system = "" } = { name: "custom" }
// name == "custom" (key exists), system == "" (default applied)

const [a = 10, b = 20, c = 30] = [1, 2]
// a == 1, b == 2, c == 30 (default applied)
```

Defaults can be combined with field renaming:

```harn
const { name: displayName = "Unknown" } = {}
// displayName == "Unknown"
```

Default expressions are evaluated fresh each time the pattern is matched
(they are not memoized). Rest patterns (`...rest`) do not support
default values.

### List destructuring

```harn
const [first, second, third] = [10, 20, 30]
// first == 10, second == 20, third == 30
```

Elements are bound positionally. If there are more bindings than elements
in the list, the excess bindings receive `nil` (unless a default value is
specified).
Use `_` to discard positions without creating a binding:

```harn
const [_, second, _] = [10, 20, 30]
// second == 20; `_` is not bound
```

### Field renaming

A dict pattern field can be renamed with `key: alias` syntax:

```harn
const {name: user_name} = {name: "Bob"}
// user_name == "Bob"
```

### Rest patterns

A `...rest` element collects remaining items into a new list or dict:

```harn
const [head, ...tail] = [1, 2, 3, 4]
// head == 1, tail == [2, 3, 4]

const {name, ...extras} = {name: "Carol", age: 25, role: "dev"}
// name == "Carol", extras == {age: 25, role: "dev"}
```

If there are no remaining items, the rest variable is bound to `[]` for
list patterns or `{}` for dict patterns. The rest element must appear
last in the pattern.

### For-in destructuring

Destructuring patterns work in `for`-`in` loops to unpack each element:

```harn
const entries = [{name: "X", val: 1}, {name: "Y", val: 2}]
for {name, val} in entries {
  log("${name}=${val}")
}

const pairs = [[1, 2], [3, 4]]
for [a, b] in pairs {
  log("${a}+${b}")
}
```

`_` is also a discard binding in loop patterns, so `for [_, value] in ...`
or `for (_, value) in ...` drops the ignored element instead of binding it.

### Var destructuring

`var` destructuring creates mutable bindings that can be reassigned:

```harn
let {x, y} = {x: 1, y: 2}
x = 10
y = 20
```

Discard bindings remain non-bindings under `var` as well: `var [_, value] =`
still only introduces `value`.

### Type errors

Destructuring a non-dict value with a dict pattern or a non-list value
with a list pattern produces a runtime error. For example,
`let {a} = "hello"` throws `"dict destructuring requires a dict value"`.

