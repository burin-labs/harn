# Builtin functions

Complete reference for all built-in functions available in Harn.

## Output

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `log(msg)` | msg: any | nil | Print with `[harn]` prefix and newline |
| `print(msg)` | msg: any | nil | Print without prefix or newline |
| `println(msg)` | msg: any | nil | Print with newline, no prefix |
| `progress(phase, message, progress?, total?)` | phase: string, message: string, optional numeric progress | nil | Emit standalone progress output. Dict options support `mode: "spinner"` with `step`, or `mode: "bar"` with `current`, `total`, and optional `width` |
| `color(text, name)` | text: any, name: string | string | Wrap text with an ANSI foreground color code |
| `bold(text)` | text: any | string | Wrap text with ANSI bold styling |
| `dim(text)` | text: any | string | Wrap text with ANSI dim styling |

## Type conversion

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `type_of(value)` | value: any | string | Returns type name: `"int"`, `"float"`, `"string"`, `"bool"`, `"nil"`, `"list"`, `"dict"`, `"closure"`, `"taskHandle"`, `"duration"`, `"enum"`, `"struct"` |
| `to_string(value)` | value: any | string | Convert to string representation |
| `to_int(value)` | value: any | int or nil | Parse/convert to integer. Floats truncate, bools become 0/1 |
| `to_float(value)` | value: any | float or nil | Parse/convert to float |
| `unreachable(value?)` | value: any (optional) | never | Throws "unreachable code was reached" at runtime. When the argument is a variable, the type checker verifies it has been narrowed to `never` (exhaustiveness check) |
| `iter(x)` | x: list, dict, set, string, generator, channel, or iter | `Iter<T>` | Lift an iterable source into a lazy, single-pass, fused iterator. No-op on an existing iter. Dict iters yield `Pair(key, value)`; string iters yield chars. See [Iterator methods](#iterator-methods) |
| `pair(a, b)` | a: any, b: any | `Pair` | Construct a two-element `Pair` value. Access via `.first` / `.second`, or destructure in a for-loop: `for (k, v) in ...` |

## Runtime shape validation

Function parameters with structural type annotations (shapes) are validated
at runtime. If a dict or struct argument is missing a required field or has
the wrong field type, a descriptive error is thrown before the function
body executes.

```harn,ignore
fn greet(u: {name: string, age: int}) {
  println("${u.name} is ${u.age}")
}

greet({name: "Alice", age: 30})   // OK
greet({name: "Alice"})            // Error: parameter 'u': missing field 'age' (int)
```

See [Error handling -- Runtime shape validation errors](error-handling.md#runtime-shape-validation-errors)
for more details.

## Result

Harn has a built-in `Result` type for representing success/failure values
without exceptions. `Ok` and `Err` create `Result.Ok` and `Result.Err`
enum variants respectively. When called on a non-Result value, `unwrap`
and `unwrap_or` pass the value through unchanged.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `Ok(value)` | value: any | Result.Ok | Create a Result.Ok value |
| `Err(value)` | value: any | Result.Err | Create a Result.Err value |
| `is_ok(result)` | result: any | bool | Returns true if value is Result.Ok |
| `is_err(result)` | result: any | bool | Returns true if value is Result.Err |
| `unwrap(result)` | result: any | any | Extract Ok value. Throws on Err. Non-Result values pass through |
| `unwrap_or(result, default)` | result: any, default: any | any | Extract Ok value. Returns default on Err. Non-Result values pass through |
| `unwrap_err(result)` | result: any | any | Extract Err value. Throws on non-Err |

Example:

```harn
let good = Ok(42)
let bad = Err("something went wrong")

println(is_ok(good))             // true
println(is_err(bad))             // true

println(unwrap(good))            // 42
println(unwrap_or(bad, 0))       // 0
println(unwrap_err(bad))         // something went wrong
```

## JSON

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `json_parse(str)` | str: string | value | Parse JSON string into Harn values. Throws on invalid JSON |
| `json_stringify(value)` | value: any | string | Serialize Harn value to JSON. Closures and handles become `null` |
| `yaml_parse(str)` | str: string | value | Parse YAML string into Harn values. Throws on invalid YAML |
| `yaml_stringify(value)` | value: any | string | Serialize Harn value to YAML |
| `toml_parse(str)` | str: string | value | Parse TOML string into Harn values. Throws on invalid TOML |
| `toml_stringify(value)` | value: any | string | Serialize Harn value to TOML |
| `json_validate(data, schema)` | data: any, schema: dict | bool | Validate data against a schema. Returns `true` if valid, throws with details if not |
| `schema_check(data, schema)` | data: any, schema: dict | Result | Validate data against an extended schema and return `Result.Ok(data)` or `Result.Err({message, errors, value?})` |
| `schema_parse(data, schema)` | data: any, schema: dict | Result | Same as `schema_check`, but applies `default` values recursively |
| `schema_is(data, schema)` | data: any, schema: dict | bool | Validate data against a schema and return `true`/`false` without throwing |
| `schema_expect(data, schema, apply_defaults?)` | data: any, schema: dict, bool (optional) | any | Validate data and return the normalized value, throwing on failure |
| `schema_from_json_schema(schema)` | schema: dict | dict | Convert a JSON Schema object into Harn's canonical schema dict |
| `schema_from_openapi_schema(schema)` | schema: dict | dict | Convert an OpenAPI Schema Object into Harn's canonical schema dict |
| `schema_to_json_schema(schema)` | schema: dict | dict | Convert an extended Harn schema into JSON Schema |
| `schema_to_openapi_schema(schema)` | schema: dict | dict | Convert an extended Harn schema into an OpenAPI-friendly schema object |
| `schema_extend(base, overrides)` | base: dict, overrides: dict | dict | Shallow-merge two schema dicts |
| `schema_partial(schema)` | schema: dict | dict | Remove `required` recursively so properties become optional |
| `schema_pick(schema, keys)` | schema: dict, keys: list | dict | Keep only selected top-level properties |
| `schema_omit(schema, keys)` | schema: dict, keys: list | dict | Remove selected top-level properties |
| `json_extract(text, key?)` | text: string, key: string (optional) | value | Extract JSON from text (strips markdown code fences). If key given, returns that key's value |

Type mapping:

| JSON | Harn |
|---|---|
| string | string |
| integer | int |
| decimal/exponent | float |
| true/false | bool |
| null | nil |
| array | list |
| object | dict |

### Canonical schema format

The canonical schema is a plain Harn dict. The validator also accepts compatible
JSON Schema / OpenAPI Schema Object spellings such as `object`, `array`,
`integer`, `number`, `boolean`, `oneOf`, `allOf`, `minLength`, `maxLength`,
`minItems`, `maxItems`, and `additionalProperties`, normalizing them into the
same internal form.

Supported canonical keys:

| Key | Type | Description |
|---|---|---|
| `type` | string | Expected type: `"string"`, `"int"`, `"float"`, `"bool"`, `"list"`, `"dict"`, `"any"` |
| `required` | list | List of required key names (for dicts) |
| `properties` | dict | Dict mapping property names to sub-schemas (for dicts) |
| `items` | dict | Schema to validate each item against (for lists) |
| `additional_properties` | bool or dict | Whether unknown dict keys are allowed, or which schema they must satisfy |

Example:

```harn
let schema = {
  type: "dict",
  required: ["name", "age"],
  properties: {
    name: {type: "string"},
    age: {type: "int"},
    tags: {type: "list", items: {type: "string"}}
  }
}
json_validate(data, schema)  // throws if invalid
```

### Extended schema constraints

The schema builtins support these additional keys:

| Key | Type | Description |
|---|---|---|
| `nullable` | bool | Allow `nil` |
| `min` / `max` | int or float | Numeric bounds |
| `min_length` / `max_length` | int | String length bounds |
| `pattern` | string | Regex pattern for strings |
| `enum` | list | Allowed literal values |
| `const` | any | Exact required literal value |
| `min_items` / `max_items` | int | List length bounds |
| `union` | list of schemas | Value must match one schema |
| `all_of` | list of schemas | Value must satisfy every schema |
| `default` | any | Default value applied by `schema_parse` |

Example:

```harn
let user_schema = {
  type: "dict",
  required: ["name", "age"],
  properties: {
    name: {type: "string", min_length: 1},
    age: {type: "int", min: 0},
    role: {type: "string", enum: ["admin", "user"], default: "user"}
  }
}

let parsed = schema_parse({name: "Ada", age: 36}, user_schema)
println(is_ok(parsed))
println(unwrap(parsed).role)
println(schema_to_json_schema(user_schema).type)
```

`schema_is(...)` is useful for dynamic checks and can participate in static
type refinement when the schema is a literal (or a variable bound from a
literal schema).

The lazy `std/schema` module provides ergonomic builders such as
`schema_string()`, `schema_object(...)`, `schema_union(...)`,
`get_typed_result(...)`, `get_typed_value(...)`, and `is_type(...)`.

Composition helpers:

```harn
let public_user = schema_pick(user_schema, ["name", "role"])
let patch_schema = schema_partial(user_schema)
let admin_user = schema_extend(user_schema, {
  properties: {
    name: {type: "string", min_length: 1},
    age: {type: "int", min: 0},
    role: {type: "string", enum: ["admin"], default: "admin"}
  }
})
```

### json_extract

Extracts JSON from LLM responses that may contain markdown code fences
or surrounding prose. Handles `` ```json ... ``` ``, `` ``` ... ``` ``,
and bare JSON with surrounding text. Uses balanced bracket matching to
correctly extract nested objects and arrays from mixed prose.

```harn
let result = llm_call("Return JSON with name and age")
let data = json_extract(result.text)         // parse, stripping fences
let name = json_extract(result.text, "name") // extract just one key
```

## Math

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `abs(n)` | n: int or float | int or float | Absolute value |
| `ceil(n)` | n: float | int | Ceiling (rounds up). Ints pass through unchanged |
| `floor(n)` | n: float | int | Floor (rounds down). Ints pass through unchanged |
| `round(n)` | n: float | int | Round to nearest integer. Ints pass through unchanged |
| `sqrt(n)` | n: int or float | float | Square root |
| `pow(base, exp)` | base: number, exp: number | int or float | Exponentiation. Returns int when both args are int and exp is non-negative |
| `min(a, b)` | a: number, b: number | int or float | Minimum of two values. Returns float if either argument is float |
| `max(a, b)` | a: number, b: number | int or float | Maximum of two values. Returns float if either argument is float |
| `random()` | none | float | Random float in [0, 1) |
| `random_int(min, max)` | min: int, max: int | int | Random integer in [min, max] inclusive |

### Trigonometry

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `sin(n)` | n: float | float | Sine (radians) |
| `cos(n)` | n: float | float | Cosine (radians) |
| `tan(n)` | n: float | float | Tangent (radians) |
| `asin(n)` | n: float | float | Inverse sine |
| `acos(n)` | n: float | float | Inverse cosine |
| `atan(n)` | n: float | float | Inverse tangent |
| `atan2(y, x)` | y: float, x: float | float | Two-argument inverse tangent |

### Logarithms and exponentials

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `log2(n)` | n: float | float | Base-2 logarithm |
| `log10(n)` | n: float | float | Base-10 logarithm |
| `ln(n)` | n: float | float | Natural logarithm |
| `exp(n)` | n: float | float | Euler's number raised to the power n |

### Constants and utilities

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `pi` | — | float | The constant pi (3.14159...) |
| `e` | — | float | Euler's number (2.71828...) |
| `sign(n)` | n: int or float | int | Sign of a number: -1, 0, or 1 |
| `is_nan(n)` | n: float | bool | Check if value is NaN |
| `is_infinite(n)` | n: float | bool | Check if value is infinite |

## Sets

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `set(items?)` | items: list (optional) | set | Create a new set, optionally from a list |
| `set_add(s, value)` | s: set, value: any | set | Add a value to a set, returns new set |
| `set_remove(s, value)` | s: set, value: any | set | Remove a value from a set, returns new set |
| `set_contains(s, value)` | s: set, value: any | bool | Check if set contains a value |
| `set_union(a, b)` | a: set, b: set | set | Union of two sets |
| `set_intersect(a, b)` | a: set, b: set | set | Intersection of two sets |
| `set_difference(a, b)` | a: set, b: set | set | Difference (elements in a but not b) |
| `set_symmetric_difference(a, b)` | a: set, b: set | set | Elements in either but not both |
| `set_is_subset(a, b)` | a: set, b: set | bool | True if all elements of a are in b |
| `set_is_superset(a, b)` | a: set, b: set | bool | True if a contains all elements of b |
| `set_is_disjoint(a, b)` | a: set, b: set | bool | True if a and b share no elements |
| `to_list(s)` | s: set | list | Convert a set to a list |

### Set methods (dot syntax)

Sets also support method syntax: `my_set.union(other)`.

| Method | Parameters | Returns | Description |
|---|---|---|---|
| `.count()` / `.len()` | none | int | Number of elements |
| `.empty()` | none | bool | True if set is empty |
| `.contains(val)` | val: any | bool | Check membership |
| `.add(val)` | val: any | set | New set with val added |
| `.remove(val)` | val: any | set | New set with val removed |
| `.union(other)` | other: set | set | Union |
| `.intersect(other)` | other: set | set | Intersection |
| `.difference(other)` | other: set | set | Elements in self but not other |
| `.symmetric_difference(other)` | other: set | set | Elements in either but not both |
| `.is_subset(other)` | other: set | bool | True if self is a subset of other |
| `.is_superset(other)` | other: set | bool | True if self is a superset of other |
| `.is_disjoint(other)` | other: set | bool | True if no shared elements |
| `.to_list()` | none | list | Convert to list |
| `.map(fn)` | fn: closure | set | Transform elements (deduplicates) |
| `.filter(fn)` | fn: closure | set | Keep elements matching predicate |
| `.any(fn)` | fn: closure | bool | True if any element matches |
| `.all(fn)` / `.every(fn)` | fn: closure | bool | True if all elements match |

## String functions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `len(value)` | value: string, list, or dict | int | Length of string (chars), list (items), or dict (keys) |
| `trim(str)` | str: string | string | Remove leading and trailing whitespace |
| `lowercase(str)` | str: string | string | Convert to lowercase |
| `uppercase(str)` | str: string | string | Convert to uppercase |
| `split(str, sep)` | str: string, sep: string | list | Split string by separator |
| `starts_with(str, prefix)` | str: string, prefix: string | bool | Check if string starts with prefix |
| `ends_with(str, suffix)` | str: string, suffix: string | bool | Check if string ends with suffix |
| `contains(str, substr)` | str: string, substr: string | bool | Check if string contains substring. Also works on lists |
| `replace(str, old, new)` | str: string, old: string, new: string | string | Replace all occurrences |
| `join(list, sep)` | list: list, sep: string | string | Join list elements with separator |
| `substring(str, start, len?)` | str: string, start: int, len: int | string | Extract substring from start position |
| `format(template, ...)` | template: string, args: any | string | Format string with `{}` placeholders. With a dict as the second arg, supports named `{key}` placeholders |

### String methods (dot syntax)

These are called on string values with dot notation: `"hello".uppercase()`.

| Method | Parameters | Returns | Description |
|---|---|---|---|
| `.trim()` | none | string | Remove leading/trailing whitespace |
| `.trim_start()` | none | string | Remove leading whitespace only |
| `.trim_end()` | none | string | Remove trailing whitespace only |
| `.lines()` | none | list | Split string by newlines |
| `.char_at(index)` | index: int | string or nil | Character at index (nil if out of bounds) |
| `.index_of(substr)` | substr: string | int | First character offset of substring (-1 if not found) |
| `.last_index_of(substr)` | substr: string | int | Last character offset of substring (-1 if not found) |
| `.lower()` / `.to_lower()` | none | string | Lowercase string |
| `.len()` | none | int | Character count |
| `.upper()` / `.to_upper()` | none | string | Uppercase string |
| `.chars()` | none | list | List of single-character strings |
| `.reverse()` | none | string | Reversed string |
| `.repeat(n)` | n: int | string | Repeat n times |
| `.pad_left(width, char?)` | width: int, char: string | string | Pad to width with char (default space) |
| `.pad_right(width, char?)` | width: int, char: string | string | Pad to width with char (default space) |

### List methods (dot syntax)

| Method | Parameters | Returns | Description |
|---|---|---|---|
| `.map(fn)` | fn: closure | list | Transform each element |
| `.filter(fn)` | fn: closure | list | Keep elements where fn returns truthy |
| `.reduce(init, fn)` | init: any, fn: closure | any | Fold with accumulator |
| `.find(fn)` | fn: closure | any or nil | First element matching predicate |
| `.find_index(fn)` | fn: closure | int | Index of first match (-1 if not found) |
| `.any(fn)` | fn: closure | bool | True if any element matches |
| `.all(fn)` / `.every(fn)` | fn: closure | bool | True if all elements match |
| `.none(fn?)` | fn: closure | bool | True if no elements match (no arg: checks emptiness) |
| `.first(n?)` | n: int (optional) | any or list | First element, or first n elements |
| `.last(n?)` | n: int (optional) | any or list | Last element, or last n elements |
| `.partition(fn)` | fn: closure | list | Split into `[[truthy], [falsy]]` |
| `.group_by(fn)` | fn: closure | dict | Group into dict keyed by fn result |
| `.sort()` / `.sort_by(fn)` | fn: closure (optional) | list | Sort (natural or by key function) |
| `.min()` / `.max()` | none | any | Minimum/maximum value |
| `.min_by(fn)` / `.max_by(fn)` | fn: closure | any | Min/max by key function |
| `.chunk(size)` | size: int | list | Split into chunks of size |
| `.window(size)` | size: int | list | Sliding windows of size |
| `.each_cons(size)` | size: int | list | Sliding windows of size |
| `.compact()` | none | list | Remove nil values |
| `.unique()` | none | list | Remove duplicates |
| `.flatten()` | none | list | Flatten one level of nesting |
| `.flat_map(fn)` | fn: closure | list | Map then flatten |
| `.tally()` | none | dict | Frequency count: `{value: count}` |
| `.zip(other)` | other: list | list | Pair elements from two lists |
| `.enumerate()` | none | list | List of `{index, value}` dicts |
| `.take(n)` / `.skip(n)` | n: int | list | First/remaining n elements |
| `.sum()` | none | int or float | Sum of numeric values |
| `.join(sep?)` | sep: string | string | Join to string |
| `.reverse()` | none | list | Reversed list |
| `.push(item)` / `.pop()` | item: any | list | New list with item added/removed (immutable) |
| `.contains(item)` | item: any | bool | Check if list contains item |
| `.index_of(item)` | item: any | int | Index of item (-1 if not found) |
| `.slice(start, end?)` | start: int, end: int | list | Slice with negative index support |

### Iterator methods

Eager list/dict/set/string methods listed above are unchanged — they
still return eager collections. Lazy iteration is opt-in via
`.iter()`, which lifts a list, dict, set, string, generator, or
channel into an `Iter<T>` value. Iterators are **single-pass, fused,
and snapshot** — they `Rc`-clone the backing collection, so mutating
the source after `.iter()` does not affect the iter.

On a dict, `.iter()` yields `Pair(key, value)` values (use `.first` /
`.second`, or destructure in a for-loop). String iteration yields
chars (Unicode scalar values).

Printing with `log(it)` renders `<iter>` or `<iter (exhausted)>` and
does **not** drain the iterator.

#### Lazy combinators (return a new `Iter`)

| Method | Parameters | Returns | Description |
|---|---|---|---|
| `.iter()` | none | `Iter<T>` | Lift a source into an iter; no-op on an existing iter |
| `.map(fn)` | fn: closure | `Iter<U>` | Lazily transform each item |
| `.filter(fn)` | fn: closure | `Iter<T>` | Lazily keep items where fn returns truthy |
| `.flat_map(fn)` | fn: closure | `Iter<U>` | Map then flatten, lazily |
| `.take(n)` | n: int | `Iter<T>` | First n items |
| `.skip(n)` | n: int | `Iter<T>` | Drop first n items |
| `.take_while(fn)` | fn: closure | `Iter<T>` | Items until predicate first returns falsy |
| `.skip_while(fn)` | fn: closure | `Iter<T>` | Drop items while predicate is truthy |
| `.zip(other)` | other: iter | `Iter<Pair<T, U>>` | Pair items from two iters, stops at shorter |
| `.enumerate()` | none | `Iter<Pair<int, T>>` | Pair each item with a 0-based index |
| `.chain(other)` | other: iter | `Iter<T>` | Yield items from self, then from other |
| `.chunks(n)` | n: int | `Iter<list<T>>` | Non-overlapping fixed-size chunks |
| `.windows(n)` | n: int | `Iter<list<T>>` | Sliding windows of size n |

#### Sinks (drain the iter, return an eager value)

| Method | Parameters | Returns | Description |
|---|---|---|---|
| `.to_list()` | none | list | Collect all items into a list |
| `.to_set()` | none | set | Collect all items into a set |
| `.to_dict()` | none | dict | Collect `Pair(key, value)` items into a dict |
| `.count()` | none | int | Count remaining items |
| `.sum()` | none | int or float | Sum of numeric items |
| `.min()` / `.max()` | none | any | Min/max item |
| `.reduce(init, fn)` | init: any, fn: closure | any | Fold with accumulator |
| `.first()` / `.last()` | none | any or nil | First/last item |
| `.any(fn)` | fn: closure | bool | True if any remaining item matches |
| `.all(fn)` | fn: closure | bool | True if all remaining items match |
| `.find(fn)` | fn: closure | any or nil | First item matching predicate |
| `.for_each(fn)` | fn: closure | nil | Invoke fn on each remaining item |

## Path functions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `dirname(path)` | path: string | string | Directory component of path |
| `basename(path)` | path: string | string | File name component of path |
| `extname(path)` | path: string | string | File extension including dot (e.g., `.harn`) |
| `path_join(parts...)` | parts: strings | string | Join path components |

## File I/O

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `read_file(path)` | path: string | string | Read entire file as UTF-8 string. Throws on failure. **Deprecated in favor of `read_file_result` for new code; the throwing form remains supported.** |
| `read_file_result(path)` | path: string | `Result<string, string>` | Non-throwing read: returns `Result.Ok(content)` on success or `Result.Err(message)` on failure. Shares `read_file`'s content cache |
| `write_file(path, content)` | path: string, content: string | nil | Write string to file. Throws on failure |
| `append_file(path, content)` | path: string, content: string | nil | Append string to file, creating it if it doesn't exist. Throws on failure |
| `copy_file(src, dst)` | src: string, dst: string | nil | Copy a file. Throws on failure |
| `delete_file(path)` | path: string | nil | Delete a file or directory (recursive). Throws on failure |
| `file_exists(path)` | path: string | bool | Check if a file or directory exists |
| `list_dir(path?)` | path: string (default `"."`) | list | List directory contents as sorted list of file names. Throws on failure |
| `mkdir(path)` | path: string | nil | Create directory and all parent directories. Throws on failure |
| `stat(path)` | path: string | dict | File metadata: `{size, is_file, is_dir, readonly, modified}`. Throws on failure |
| `temp_dir()` | none | string | System temporary directory path |
| `render(path, bindings?)` | path: string, bindings: dict | string | Read a template file relative to the current module's asset root and render it. The template language supports `{{ name }}` interpolation (with nested paths and filters), `{{ if }} / {{ elif }} / {{ else }} / {{ end }}`, `{{ for item in xs }} ... {{ end }}` (with `{{ loop.index }}` etc.), `{{ include "..." }}` partials, `{{# comments #}}`, `{{ raw }} ... {{ endraw }}` verbatim blocks, and `{{- -}}` whitespace trim markers. See the [Prompt templating reference](./prompt-templating.md) for the full grammar and filter list. When called from an imported module, resolves relative to that module's directory, not the entry pipeline. Without bindings, just reads the file |
| `render_prompt(path, bindings?)` | path: string, bindings: dict | string | Prompt-oriented alias of `render(...)`. Use this for `.harn.prompt` / `.prompt` assets when you want the asset to be surfaced explicitly in bundle manifests and preflight output |

## Environment and system

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `env(name)` | name: string | string or nil | Read environment variable |
| `env_or(name, default)` | name: string, default: any | string or default | Read environment variable, or return `default` when unset. One-line replacement for the common `let v = env(K); if v { v } else { default }` pattern |
| `timestamp()` | none | float | Unix timestamp in seconds with sub-second precision |
| `elapsed()` | none | int | Milliseconds since VM startup |
| `exec(cmd, args...)` | cmd: string, args: strings | dict | Execute external command. Returns `{stdout, stderr, status, success}` |
| `exec_at(dir, cmd, args...)` | dir: string, cmd: string, args: strings | dict | Execute external command inside a specific directory |
| `shell(cmd)` | cmd: string | dict | Execute command via shell. Returns `{stdout, stderr, status, success}` |
| `shell_at(dir, cmd)` | dir: string, cmd: string | dict | Execute shell command inside a specific directory |
| `exit(code)` | code: int (default 0) | never | Terminate the process |
| `username()` | none | string | Current OS username |
| `hostname()` | none | string | Machine hostname |
| `platform()` | none | string | OS name: `"darwin"`, `"linux"`, or `"windows"` |
| `arch()` | none | string | CPU architecture (e.g., `"aarch64"`, `"x86_64"`) |
| `uuid()` | none | string | Generate a random v4 UUID |
| `home_dir()` | none | string | User's home directory path |
| `pid()` | none | int | Current process ID |
| `cwd()` | none | string | Current working directory |
| `execution_root()` | none | string | Directory used for source-relative execution helpers such as `exec_at(...)` / `shell_at(...)` |
| `asset_root()` | none | string | Directory used for source-relative asset helpers such as `render(...)` / `render_prompt(...)` |
| `source_dir()` | none | string | Directory of the currently-executing `.harn` file (falls back to cwd) |
| `project_root()` | none | string or nil | Nearest ancestor directory containing `harn.toml` |
| `runtime_paths()` | none | dict | Resolved runtime path model: `{execution_root, asset_root, state_root, run_root, worktree_root}` |
| `date_iso()` | none | string | Current UTC time in ISO 8601 format (e.g., `"2026-03-29T14:30:00.123Z"`) |

## Regular expressions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `regex_match(pattern, text)` | pattern: string, text: string | list or nil | Find all non-overlapping matches. Returns nil if no matches |
| `regex_replace(pattern, replacement, text)` | pattern: string, replacement: string, text: string | string | Replace all matches. Throws on invalid regex |
| `regex_captures(pattern, text)` | pattern: string, text: string | list | Find all matches with capture group details |

### regex_captures

Returns a list of dicts, one per match. Each dict contains:

- `match` -- the full matched string
- `groups` -- a list of positional capture group values (from `(...)`)
- Named capture groups (from `(?P<name>...)`) appear as additional keys

```harn
let results = regex_captures("(\\w+)@(\\w+)", "alice@example bob@test")
// [
//   {match: "alice@example", groups: ["alice", "example"]},
//   {match: "bob@test", groups: ["bob", "test"]}
// ]
```

Named capture groups are added as top-level keys on each result dict:

```harn
let named = regex_captures("(?P<user>\\w+):(?P<role>\\w+)", "alice:admin")
// [{match: "alice:admin", groups: ["alice", "admin"], user: "alice", role: "admin"}]
```

Returns an empty list if there are no matches. Throws on invalid regex.

## Encoding

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `base64_encode(string)` | string: string | string | Base64 encode a string (standard alphabet with padding) |
| `base64_decode(string)` | string: string | string | Base64 decode a string. Throws on invalid input |
| `url_encode(string)` | string: string | string | URL percent-encode a string. Unreserved characters (alphanumeric, `-`, `_`, `.`, `~`) pass through unchanged |
| `url_decode(string)` | string: string | string | Decode a URL-encoded string. Decodes `%XX` sequences and `+` as space |

Example:

```harn
let encoded = base64_encode("Hello, World!")
println(encoded)                  // SGVsbG8sIFdvcmxkIQ==
println(base64_decode(encoded))   // Hello, World!
```

```harn
println(url_encode("hello world"))         // hello%20world
println(url_decode("hello%20world"))       // hello world
println(url_encode("a=1&b=2"))             // a%3D1%26b%3D2
println(url_decode("hello+world"))         // hello world
```

## Hashing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `sha256(string)` | string: string | string | SHA-256 hash, returned as a lowercase hex-encoded string |
| `md5(string)` | string: string | string | MD5 hash, returned as a lowercase hex-encoded string |

Example:

```harn
println(sha256("hello"))  // 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
println(md5("hello"))     // 5d41402abc4b2a76b9719d911017c592
```

## Date/Time

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `date_now()` | none | dict | Current UTC datetime as dict with `year`, `month`, `day`, `hour`, `minute`, `second`, `weekday`, and `timestamp` fields |
| `date_parse(str)` | str: string | float | Parse a datetime string (e.g., `"2024-01-15 10:30:00"`) into a Unix timestamp. Extracts numeric components from the string. Throws if fewer than 3 parts (year, month, day). Validates month (1-12), day (1-31), hour (0-23), minute (0-59), second (0-59) |
| `date_format(dt, format?)` | dt: float, int, or dict; format: string (default `"%Y-%m-%d %H:%M:%S"`) | string | Format a timestamp or date dict as a string. Supports `%Y`, `%m`, `%d`, `%H`, `%M`, `%S` placeholders. Throws for negative timestamps |

## Testing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `assert(condition, msg?)` | condition: any, msg: string (optional) | nil | Assert value is truthy. Throws with message on failure |
| `assert_eq(a, b, msg?)` | a: any, b: any, msg: string (optional) | nil | Assert two values are equal. Throws with message on failure |
| `assert_ne(a, b, msg?)` | a: any, b: any, msg: string (optional) | nil | Assert two values are not equal. Throws with message on failure |

## HTTP

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `http_get(url, options?)` | url: string, options: dict | dict | GET request |
| `http_post(url, body, options?)` | url: string, body: string, options: dict | dict | POST request |
| `http_put(url, body, options?)` | url: string, body: string, options: dict | dict | PUT request |
| `http_patch(url, body, options?)` | url: string, body: string, options: dict | dict | PATCH request |
| `http_delete(url, options?)` | url: string, options: dict | dict | DELETE request |
| `http_request(method, url, options?)` | method: string, url: string, options: dict | dict | Generic HTTP request |

All HTTP functions return `{status: int, headers: dict, body: string, ok: bool}`.
Options: `timeout` (ms), `retries`, `backoff` (ms), `headers` (dict),
`auth` (string or `{bearer: "token"}` or `{basic: {user, password}}`),
`follow_redirects` (bool), `max_redirects` (int), `body` (string).
Throws on network errors.

### Mock HTTP

For testing pipelines that make HTTP calls without hitting real servers.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `http_mock(method, url_pattern, response)` | method: string, url_pattern: string, response: dict | nil | Register a mock. Use `*` in url_pattern for glob matching (supports multiple `*` wildcards, e.g., `https://api.example.com/*/items/*`) |
| `http_mock_clear()` | none | nil | Clear all mocks and recorded calls |
| `http_mock_calls()` | none | list | Return list of `{method, url, body}` for all intercepted calls |

```harn
http_mock("GET", "https://api.example.com/users", {
  status: 200,
  body: "{\"users\": [\"alice\"]}",
  headers: {}
})
let resp = http_get("https://api.example.com/users")
assert_eq(resp.status, 200)
```

## Interactive input

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `prompt_user(msg)` | msg: string (optional) | string | Display message, read line from stdin |

## Host interop

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `host_call(name, args)` | name: string, args: any | any | Call a host capability operation using `capability.operation` naming |
| `host_capabilities()` | — | dict | Typed host capability manifest |
| `host_has(capability, op?)` | capability: string, op: string | bool | Check whether a typed host capability/operation exists |
| `host_mock(capability, op, response_or_config, params?)` | capability: string, op: string, response_or_config: any or dict, params: dict | nil | Register a runtime mock for a typed host operation |
| `host_mock_clear()` | — | nil | Clear registered typed host mocks and recorded mock invocations |
| `host_mock_calls()` | — | list | Return recorded typed host mock invocations |

`host_capabilities()` returns the capability manifest surfaced by the active
host bridge. The local runtime exposes generic `process`, `template`, and
`interaction` capabilities. Product hosts can add capabilities such as
`workspace`, `project`, `runtime`, `editor`, `git`, or `diagnostics`.

Prefer `host_call("capability.operation", args)` in shared wrappers and
host-owned `.harn` modules so capability names stay consistent across the
runtime, host manifest, and preflight validation.

`host_mock(...)` is intended for tests and local conformance runs. The third
argument may be either a direct result value or a config dict containing
`result`, `params`, and/or `error`. Mock matching is last-write-wins and only
requires the declared `params` subset to match the actual host call
call. Matched calls are recorded in `host_mock_calls()` as
`{capability, operation, params}` dictionaries.

For higher-level test helpers, import `std/testing`:

```harn
import {
  assert_host_called,
  clear_host_mocks,
  mock_host_error,
  mock_host_result,
} from "std/testing"

clear_host_mocks()
mock_host_result("project", "metadata_get", "hello", {dir: ".", namespace: "facts"})
assert_eq(host_call("project.metadata_get", {dir: ".", namespace: "facts"}), "hello")
assert_host_called("project", "metadata_get", {dir: ".", namespace: "facts"}, nil)

mock_host_error("project", "scan", "scan failed", nil)
let result = try { host_call("project.scan", {}) }
assert(is_err(result))
```

## Async and timing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `sleep(duration)` | duration: int (ms) or duration literal | nil | Pause execution |

## Concurrency primitives

### Channels

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `channel(name?)` | name: string (default `"default"`) | dict | Create a channel with `name`, `type`, and `messages` fields |
| `send(ch, value)` | ch: dict, value: any | nil | Send a value to a channel |
| `receive(ch)` | ch: dict | any | Receive a value from a channel (blocks until data available) |
| `close_channel(ch)` | ch: channel | nil | Close a channel, preventing further sends |
| `try_receive(ch)` | ch: channel | any or nil | Non-blocking receive. Returns nil if no data available |
| `select(ch1, ch2, ...)` | channels: channel | dict or nil | Wait for data on any channel. Returns `{index, value, channel}` for the first ready channel, or nil if all closed |

### Atomics

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `atomic(initial?)` | initial: any (default 0) | dict | Create an atomic value |
| `atomic_get(a)` | a: dict | any | Read the current value |
| `atomic_set(a, value)` | a: dict, value: any | int | Set value, returns previous value |
| `atomic_add(a, delta)` | a: dict, delta: int | int | Add delta, returns previous value |
| `atomic_cas(a, expected, new)` | a: dict, expected: int, new: int | bool | Compare-and-swap. Returns true if the swap succeeded |

## Persistent store

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `store_get(key)` | key: string | any | Retrieve value from store, nil if missing |
| `store_set(key, value)` | key: string, value: any | nil | Store value, auto-saves to `.harn/store.json` |
| `store_delete(key)` | key: string | nil | Remove key from store |
| `store_list()` | none | list | List all keys (sorted) |
| `store_save()` | none | nil | Explicitly flush store to disk |
| `store_clear()` | none | nil | Remove all keys from store |

The store is backed by `.harn/store.json` relative to the script's
directory. The file is created lazily on first `store_set`. In bridge mode,
the host can override these builtins.

## LLM

See [LLM calls and agent loops](llm-and-agents.md) for full documentation.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `llm_call(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Single LLM request. Returns `{text, model, input_tokens, output_tokens}`. Throws on transport / rate-limit / schema-validation failures |
| `llm_call_safe(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Non-throwing envelope around `llm_call`. Returns `{ok: bool, response: dict or nil, error: {category, message} or nil}`. `error.category` is one of `ErrorCategory`'s canonical strings (`"rate_limit"`, `"timeout"`, `"overloaded"`, `"server_error"`, `"transient_network"`, `"schema_validation"`, `"auth"`, `"not_found"`, `"circuit_open"`, `"tool_error"`, `"tool_rejected"`, `"cancelled"`, `"generic"`) |
| `with_rate_limit(provider, fn, options?)` | provider: string, fn: closure, options: dict | whatever `fn` returns | Acquire a permit from the provider's sliding-window rate limiter, invoke `fn`, and retry with exponential backoff on retryable errors (`rate_limit`, `overloaded`, `transient_network`, `timeout`). Options: `max_retries` (default 5), `backoff_ms` (default 1000, capped at 30s after doubling) |
| `llm_completion(prefix, suffix?, system?, options?)` | prefix: string, suffix: string, system: string, options: dict | dict | Text completion / fill-in-the-middle request. Returns `{text, model, input_tokens, output_tokens}` |
| `agent_loop(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Multi-turn agent loop with `##DONE##` sentinel, daemon/idling support, and optional per-turn context filtering. Returns `{status, text, iterations, duration_ms, tools_used}` |
| `llm_info()` | — | dict | Current LLM config: `{provider, model, api_key_set}` |
| `llm_usage()` | — | dict | Cumulative usage: `{input_tokens, output_tokens, total_duration_ms, call_count, total_calls}` |
| `llm_resolve_model(alias)` | alias: string | dict | Resolve model alias to `{id, provider}` via providers.toml |
| `llm_pick_model(target, options?)` | target: string, options: dict | dict | Resolve a model alias or tier to `{id, provider, tier}` |
| `llm_infer_provider(model_id)` | model_id: string | string | Infer provider from model ID (e.g. `"claude-*"` → `"anthropic"`) |
| `llm_model_tier(model_id)` | model_id: string | string | Get capability tier: `"small"`, `"mid"`, or `"frontier"` |
| `llm_healthcheck(provider?)` | provider: string | dict | Validate API key. Returns `{valid, message, metadata}` |
| `llm_rate_limit(provider, options?)` | provider: string, options: dict | int/nil/bool | Set (`{rpm: N}`), query, or clear (`{rpm: 0}`) per-provider rate limit |
| `llm_providers()` | — | list | List all configured provider names |
| `llm_config(provider?)` | provider: string | dict | Get provider config (base_url, auth_style, etc.) |
| `llm_cost(model, input_tokens, output_tokens)` | model: string, input_tokens: int, output_tokens: int | float | Estimate USD cost from embedded pricing table |
| `llm_session_cost()` | — | dict | Session totals: `{total_cost, input_tokens, output_tokens, call_count}` |
| `llm_budget(max_cost)` | max_cost: float | nil | Set session budget in USD. LLM calls throw if exceeded |
| `llm_budget_remaining()` | — | float or nil | Remaining budget (nil if no budget set) |
| `llm_mock(response)` | response: dict | nil | Queue a mock LLM response. Dict supports `text`, `tool_calls`, `match` (glob), `input_tokens`, `output_tokens`, `thinking`, `stop_reason`, `model`, `error: {category, message}` (short-circuits the call and surfaces as `VmError::CategorizedError` — useful for testing `llm_call_safe` envelopes and `with_rate_limit` retry loops) |
| `llm_mock_calls()` | — | list | Return list of `{messages, system, tools}` for all calls made to the mock provider |
| `llm_mock_clear()` | — | nil | Clear all queued mock responses and recorded calls |

FIFO mocks (no `match` field) are consumed in order. Pattern-matched mocks
(with `match`) persist and match against the last user message content using
glob patterns. When no mocks match, the default deterministic mock behavior
is used.

```harn
// Queue specific responses for the mock provider
llm_mock({text: "The answer is 42."})
llm_mock({
  text: "Let me check that.",
  tool_calls: [{name: "read_file", arguments: {path: "main.rs"}}],
})
let r = llm_call("question", nil, {provider: "mock"})
assert_eq(r.text, "The answer is 42.")

// Pattern-matched mocks (reusable, not consumed)
llm_mock({text: "Hello!", match: "*greeting*"})

// Error injection for testing resilient code paths. The mock
// surfaces as a real `VmError::CategorizedError`, so `error_category`,
// `try { ... } catch`, `llm_call_safe`, and `with_rate_limit` all see
// it the same way they would a live provider failure.
llm_mock({error: {category: "rate_limit", message: "429 Too Many Requests"}})

// Inspect what was sent
let calls = llm_mock_calls()
llm_mock_clear()
```

### Transcript helpers

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `transcript(metadata?)` | metadata: dict | dict | Create a new transcript |
| `transcript_from_messages(messages_or_transcript)` | list or dict | dict | Normalize a message list into a transcript |
| `transcript_messages(transcript)` | transcript: dict | list | Get transcript messages |
| `transcript_summary(transcript)` | transcript: dict | string or nil | Get transcript summary |
| `transcript_id(transcript)` | transcript: dict | string | Get transcript id |
| `transcript_export(transcript)` | transcript: dict | string | Export transcript as JSON |
| `transcript_import(json_text)` | json_text: string | dict | Import transcript JSON |
| `transcript_fork(transcript, options?)` | transcript: dict, options: dict | dict | Fork transcript, optionally dropping messages or summary |
| `transcript_summarize(transcript, options?)` | transcript: dict, options: dict | dict | Summarize and compact a transcript via `llm_call` |
| `transcript_compact(transcript, options?)` | transcript: dict, options: dict | dict | Compact a transcript locally, preserving summary and recent turns |
| `transcript_auto_compact(messages, options?)` | messages: list, options: dict | list | Apply the agent-loop compaction pipeline to a message list using `llm`, `truncate`, or `custom` strategy |

### Provider configuration

LLM provider endpoints, model aliases, inference rules, and default parameters
are configured via a TOML file. The VM searches for config in this order:

1. `HARN_PROVIDERS_CONFIG` env var (explicit path)
2. `~/.config/harn/providers.toml`
3. Built-in defaults (Anthropic, OpenAI, OpenRouter, HuggingFace, Ollama, Local)

See `harn init` to generate a default config file, or create one manually:

```toml
[providers.anthropic]
base_url = "https://api.anthropic.com/v1"
auth_style = "header"
auth_header = "x-api-key"
auth_env = "ANTHROPIC_API_KEY"
chat_endpoint = "/messages"

[providers.local]
base_url = "http://localhost:8000"
base_url_env = "LOCAL_LLM_BASE_URL"
auth_style = "none"
chat_endpoint = "/v1/chat/completions"
completion_endpoint = "/v1/completions"

[aliases]
sonnet = { id = "claude-sonnet-4-20250514", provider = "anthropic" }

[[inference_rules]]
pattern = "claude-*"
provider = "anthropic"

[[tier_rules]]
pattern = "claude-*"
tier = "frontier"

[model_defaults."qwen/*"]
temperature = 0.3
```

## Timers

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `timer_start(name?)` | name: string | dict | Start a named timer |
| `timer_end(timer)` | timer: dict | int | Stop timer, prints elapsed, returns milliseconds |
| `elapsed()` | — | int | Milliseconds since process start |

## Circuit breakers

Protect against cascading failures by tracking error counts and opening
a circuit when a threshold is reached.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `circuit_breaker(name, threshold?, reset_ms?)` | name: string, threshold: int (default 5), reset_ms: int (default 30000) | string | Create a named circuit breaker. Returns the name |
| `circuit_check(name)` | name: string | string | Check state: `"closed"`, `"open"`, or `"half_open"` (after reset period) |
| `circuit_record_failure(name)` | name: string | bool | Record a failure. Returns true if the circuit just opened |
| `circuit_record_success(name)` | name: string | nil | Record a success, resetting failure count and closing the circuit |
| `circuit_reset(name)` | name: string | nil | Manually reset the circuit to closed |

Example:

```harn
circuit_breaker("api", 3, 10000)

for i in 0 to 5 exclusive {
  if circuit_check("api") == "open" {
    println("circuit open, skipping call")
  } else {
    try {
      let resp = http_get("https://api.example.com/data")
      circuit_record_success("api")
    } catch e {
      circuit_record_failure("api")
    }
  }
}
```

## Tracing

Distributed tracing primitives for instrumenting pipeline execution.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `trace_start(name)` | name: string | dict | Start a trace span. Returns a span dict with `trace_id`, `span_id`, `name`, `start_ms` |
| `trace_end(span)` | span: dict | nil | End a span and emit a structured log line with duration |
| `trace_id()` | none | string or nil | Current trace ID from the span stack, or nil if no active span |
| `enable_tracing(enabled?)` | enabled: bool (default true) | nil | Enable or disable pipeline-level tracing |
| `trace_spans()` | none | list | Peek at recorded trace spans |
| `trace_summary()` | none | string | Formatted summary of trace spans |

Example:

```harn
let span = trace_start("fetch_data")
// ... do work ...
trace_end(span)

println(trace_summary())
```

### Agent trace events

Fine-grained agent loop trace events for observability and debugging.
Events are collected during `agent_loop` execution and can be inspected
after the loop completes.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `agent_trace()` | none | list | Peek at collected agent trace events. Each event is a dict with a `type` field (`llm_call`, `tool_execution`, `tool_rejected`, `loop_intervention`, `context_compaction`, `phase_change`, `loop_complete`) and type-specific fields |
| `agent_trace_summary()` | none | dict | Rolled-up summary of agent trace events with aggregated token counts, durations, tool usage, and iteration counts |

Example:

```harn,ignore
let result = agent_loop("summarize this file", tools: [read_file])
let summary = agent_trace_summary()
println("LLM calls: " + str(summary.llm_calls))
println("Tools used: " + str(summary.tools_used))
```

## Error classification

Structured error throwing and classification for retry logic and error handling.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `throw_error(message, category?)` | message: string, category: string | never | Throw a categorized error. The error is a dict with `message` and `category` fields |
| `error_category(err)` | err: any | string | Extract category from a caught error. Returns `"timeout"`, `"auth"`, `"rate_limit"`, `"tool_error"`, `"cancelled"`, `"not_found"`, `"circuit_open"`, or `"generic"` |
| `is_timeout(err)` | err: any | bool | Check if error is a timeout |
| `is_rate_limited(err)` | err: any | bool | Check if error is a rate limit |

Example:

```harn
try {
  throw_error("request timed out", "timeout")
} catch e {
  if is_timeout(e) {
    println("will retry after backoff")
  }
  println(error_category(e))  // "timeout"
}
```

## Tool registry (low-level)

Low-level tool management functions for building and inspecting tool
registries programmatically. For MCP serving, see the `tool_define` /
`mcp_tools` API above.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `tool_remove(registry, name)` | registry, name: string | dict | Remove a tool by name |
| `tool_list(registry)` | registry: dict | list | List tools as `[{name, description, parameters}]` |
| `tool_find(registry, name)` | registry, name: string | dict or nil | Find a tool entry by name |
| `tool_select(registry, names)` | registry: dict, names: list | dict | Return a registry containing only the named tools |
| `tool_count(registry)` | registry: dict | int | Number of tools in the registry |
| `tool_describe(registry)` | registry: dict | string | Human-readable summary of all tools |
| `tool_schema(registry, components?)` | registry, components: dict | dict | Generate JSON Schema for all tools |
| `tool_prompt(registry)` | registry: dict | string | Generate an LLM system prompt describing available tools |
| `tool_parse_call(text)` | text: string | list | Parse `<tool_call>...</tool_call>` XML from LLM output |
| `tool_format_result(name, result)` | name, result: string | string | Format a `<tool_result>` XML envelope |

## Structured logging

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `log_json(key, value)` | key: string, value: any | nil | Emit a JSON log line with timestamp |

## Metadata

Project metadata store backed by host-managed sharded JSON files.
Supports hierarchical namespace resolution (child directories inherit
from parents). The default filesystem backend persists namespace shards
under `.harn/metadata/<namespace>/entries.json` and still reads the legacy
monolithic `root.json` shard.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `metadata_get(dir, namespace?)` | dir: string, namespace: string | dict \| nil | Read metadata with inheritance |
| `metadata_resolve(dir, namespace?)` | dir: string, namespace: string | dict \| nil | Read resolved metadata while preserving namespaces |
| `metadata_entries(namespace?)` | namespace: string | list | List stored directories with local and resolved metadata |
| `metadata_set(dir, namespace, data)` | dir: string, namespace: string, data: dict | nil | Write metadata for directory/namespace |
| `metadata_save()` | — | nil | Flush metadata to disk |
| `metadata_stale(project)` | project: string | dict | Check staleness: `{any_stale, tier1, tier2}` |
| `metadata_status(namespace?)` | namespace: string | dict | Summarize directory counts, namespaces, missing hashes, and stale state |
| `metadata_refresh_hashes()` | — | nil | Recompute content hashes |
| `compute_content_hash(dir)` | dir: string | string | Hash of directory contents |
| `invalidate_facts(dir)` | dir: string | nil | Mark cached facts as stale |
| `scan_directory(path?, pattern_or_options?, options?)` | path: string, pattern: string or options: dict | list | Enumerate files and directories with optional `pattern`, `max_depth`, `include_hidden`, `include_dirs`, `include_files` |

## MCP (Model Context Protocol)

Connect to external tool servers using the
[Model Context Protocol](https://modelcontextprotocol.io). Harn supports
stdio transport (spawns a child process) and HTTP transport for remote
MCP servers.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `mcp_connect(command, args?)` | command: string, args: list | mcp\_client | Spawn an MCP server and perform the initialize handshake |
| `mcp_list_tools(client)` | client: mcp\_client | list | List available tools from the server |
| `mcp_call(client, name, arguments?)` | client: mcp\_client, name: string, arguments: dict | string or list | Call a tool and return the result |
| `mcp_list_resources(client)` | client: mcp\_client | list | List available resources from the server |
| `mcp_list_resource_templates(client)` | client: mcp\_client | list | List resource templates (URI templates) from the server |
| `mcp_read_resource(client, uri)` | client: mcp\_client, uri: string | string or list | Read a resource by URI |
| `mcp_list_prompts(client)` | client: mcp\_client | list | List available prompts from the server |
| `mcp_get_prompt(client, name, arguments?)` | client: mcp\_client, name: string, arguments: dict | dict | Get a prompt with optional arguments |
| `mcp_server_info(client)` | client: mcp\_client | dict | Get connection info (`name`, `connected`) |
| `mcp_disconnect(client)` | client: mcp\_client | nil | Kill the server process and release resources |

Example:

```harn
let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
let tools = mcp_list_tools(client)
println(tools)

let result = mcp_call(client, "read_file", {"path": "/tmp/hello.txt"})
println(result)

mcp_disconnect(client)
```

Notes:

- `mcp_call` returns a string when the tool produces a single text block,
  a list of content dicts for multi-block results, or nil when empty.
- If the tool reports `isError: true`, `mcp_call` throws the error text.
- `mcp_connect` throws if the command cannot be spawned or the initialize
  handshake fails.

### Auto-connecting MCP servers via harn.toml

Instead of calling `mcp_connect` manually, you can declare MCP servers in
`harn.toml`. They will be connected automatically before the pipeline executes
and made available through the global `mcp` dict.

Add a `[[mcp]]` entry for each server:

```toml
[[mcp]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[[mcp]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
```

Each entry requires:

| Field | Type | Description |
|---|---|---|
| `name` | string | Identifier used to access the client (e.g., `mcp.filesystem`) |
| `command` | string | Executable to spawn for stdio transports |
| `args` | list of strings | Command-line arguments for stdio transports (default: empty) |
| `transport` | string | `stdio` (default) or `http` |
| `url` | string | Remote MCP server URL for HTTP transports |
| `auth_token` | string | Optional explicit bearer token for HTTP transports |
| `client_id` | string | Optional pre-registered OAuth client ID for HTTP transports |
| `client_secret` | string | Optional pre-registered OAuth client secret |
| `scopes` | string | Optional OAuth scope string for login/consent |
| `protocol_version` | string | Optional MCP protocol version override |

The connected clients are available as properties on the `mcp` global dict:

```harn
pipeline default() {
  let tools = mcp_list_tools(mcp.filesystem)
  println(tools)

  let result = mcp_call(mcp.github, "list_issues", {repo: "harn"})
  println(result)
}
```

If a server fails to connect, a warning is printed to stderr and that
server is omitted from the `mcp` dict. Other servers still connect
normally. The `mcp` global is only defined when at least one server
connects successfully.

For HTTP MCP servers, use the CLI to establish OAuth once and let Harn
reuse the stored token automatically:

```bash
harn mcp redirect-uri
harn mcp login notion
```

### MCP server mode

Harn pipelines can expose tools, resources, resource templates, and prompts
as an MCP server using `harn mcp-serve`. The CLI serves them over stdio
using the MCP protocol, making them callable by Claude Desktop, Cursor,
or any MCP client.

**Declarative syntax** (preferred):

```harn
tool greet(name: string) -> string {
  description "Greet someone by name"
  "Hello, " + name + "!"
}
```

The `tool` keyword declares a tool with typed parameters, an optional
description, and a body. Parameter types map to JSON Schema
(`string` -> `"string"`, `int` -> `"integer"`, `float` -> `"number"`,
`bool` -> `"boolean"`). Parameters with default values are emitted as
optional schema fields (`required: false`) and carry their `default`
value into the generated tool registry entry. Each `tool` declaration produces its own
tool registry dict.

**Programmatic API**:

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `tool_registry()` | — | dict | Create an empty tool registry |
| `tool_define(registry, name, desc, config)` | registry, name, desc: string, config: dict | dict | Add a tool (config: `{parameters, handler, returns?, annotations?, ...}`) |
| `mcp_tools(registry)` | registry: dict | nil | Register tools for MCP serving |
| `mcp_resource(config)` | config: dict | nil | Register a static resource (`{uri, name, text, description?, mime_type?}`) |
| `mcp_resource_template(config)` | config: dict | nil | Register a resource template (`{uri_template, name, handler, description?, mime_type?}`) |
| `mcp_prompt(config)` | config: dict | nil | Register a prompt (`{name, handler, description?, arguments?}`) |

Tool annotations (MCP spec `annotations` field) can be passed in the
`tool_define` config to describe tool behavior:

```harn
tools = tool_define(tools, "search", "Search files", {
  parameters: { query: {type: "string"} },
  returns: {type: "string"},
  handler: { args -> "results for ${args.query}" },
  annotations: {
    title: "File Search",
    readOnlyHint: true,
    destructiveHint: false
  }
})
```

Unknown `tool_define` config keys are preserved on the tool entry. Workflow
graphs use this to carry runtime policy metadata directly on a tool registry,
for example:

```harn
tools = tool_define(tools, "read", "Read files", {
  parameters: { path: {type: "string"} },
  returns: {type: "string"},
  handler: nil,
  policy: {
    capabilities: {workspace: ["read_text"]},
    side_effect_level: "read_only",
    path_params: ["path"],
    mutation_classification: "read_only"
  }
})
```

When a workflow node uses that registry, Harn intersects the declared tool
policy with the graph, node, and host ceilings during validation and at
execution time.

### Declarative tool approval

`agent_loop`, `workflow_execute`, and workflow stage nodes accept an
`approval_policy` option that declaratively gates tool calls:

```harn
agent_loop("task", "system", {
  approval_policy: {
    auto_approve: ["read*", "list_*"],
    auto_deny: ["shell*"],
    require_approval: ["edit_*", "write_*"],
    write_path_allowlist: ["/workspace/**"]
  }
})
```

Evaluation order: `auto_deny` → `write_path_allowlist` → `auto_approve` →
`require_approval`. Tools that match no pattern default to `AutoApproved`.
`require_approval` calls the host via the canonical ACP
`session/request_permission` request and **fails closed** if the host
does not implement it. Policies compose
across nested scopes with most-restrictive intersection: auto-deny and
require-approval take the union, while `auto_approve` and
`write_path_allowlist` take the intersection.

Example (`agent.harn`):

```harn
pipeline main(task) {
  var tools = tool_registry()
  tools = tool_define(tools, "greet", "Greet someone", {
    parameters: { name: {type: "string"} },
    returns: {type: "string"},
    handler: { args -> "Hello, ${args.name}!" }
  })
  mcp_tools(tools)

  mcp_resource({
    uri: "docs://readme",
    name: "README",
    text: "# My Agent\nA demo MCP server."
  })

  mcp_resource_template({
    uri_template: "config://{key}",
    name: "Config Values",
    handler: { args -> "value for ${args.key}" }
  })

  mcp_prompt({
    name: "review",
    description: "Code review prompt",
    arguments: [{ name: "code", required: true }],
    handler: { args -> "Please review:\n${args.code}" }
  })
}
```

Run as an MCP server:

```bash
harn mcp-serve agent.harn
```

Configure in Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "my-agent": {
      "command": "harn",
      "args": ["mcp-serve", "agent.harn"]
    }
  }
}
```

Notes:

- `mcp_tools(registry)` (or the alias `mcp_serve`) must be called to register tools.
- Resources, resource templates, and prompts are registered individually.
- All `print`/`println` output goes to stderr (stdout is the MCP transport).
- The server supports the `2025-11-25` MCP protocol version over stdio.
- Tool handlers receive arguments as a dict and should return a string result.
- Prompt handlers receive arguments as a dict and return a string (single
  user message) or a list of `{role, content}` dicts.
- Resource template handlers receive URI template variables as a dict and
  return the resource text.

## Workflow and orchestration builtins

These builtins expose Harn's typed orchestration runtime.

### Workflow graph and planning

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `workflow_graph(config)` | config: dict | workflow graph | Normalize a workflow definition into the typed workflow IR |
| `workflow_validate(graph, ceiling?)` | graph: workflow, ceiling: dict (optional) | dict | Validate graph structure and capability ceilings |
| `workflow_inspect(graph, ceiling?)` | graph: workflow, ceiling: dict (optional) | dict | Return graph plus validation summary |
| `workflow_clone(graph)` | graph: workflow | workflow graph | Clone a workflow and append an audit entry |
| `workflow_insert_node(graph, node, edge?)` | graph, node, edge | workflow graph | Insert a node and optional edge |
| `workflow_replace_node(graph, node_id, node)` | graph, node_id, node | workflow graph | Replace a node definition |
| `workflow_rewire(graph, from, to, branch?)` | graph, from, to, branch | workflow graph | Rewire an edge |
| `workflow_set_model_policy(graph, node_id, policy)` | graph, node_id, policy | workflow graph | Set per-node model policy |
| `workflow_set_context_policy(graph, node_id, policy)` | graph, node_id, policy | workflow graph | Set per-node context policy |
| `workflow_set_auto_compact(graph, node_id, policy)` | graph, node_id, policy | workflow graph | Set per-node auto-compaction policy |
| `workflow_set_output_visibility(graph, node_id, visibility)` | graph, node_id, visibility | workflow graph | Set per-node output-visibility filter (`"public"`/`"public_only"`/nil) |
| `workflow_policy_report(graph, ceiling?)` | graph, ceiling: dict (optional) | dict | Inspect workflow/node policies against an explicit or builtin ceiling |
| `workflow_diff(left, right)` | left, right | dict | Compare two workflow graphs |
| `workflow_commit(graph, reason?)` | graph, reason | workflow graph | Validate and append a commit audit entry |

### Workflow execution and run records

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `workflow_execute(task, graph, artifacts?, options?)` | task, graph, artifacts, options | dict | Execute a workflow and persist a run record |
| `run_record(payload)` | payload: dict | run record | Normalize a run record |
| `run_record_save(run, path?)` | run, path | dict | Persist a run record |
| `run_record_load(path)` | path: string | run record | Load a run record from disk |
| `load_run_tree(path)` | path: string | dict | Load a persisted run with delegated child-run lineage |
| `run_record_fixture(run)` | run | replay fixture | Derive a replay/eval fixture from a saved run |
| `run_record_eval(run, fixture?)` | run, fixture | dict | Evaluate a run against an embedded or explicit fixture |
| `run_record_eval_suite(cases)` | cases: list | dict | Evaluate a list of `{run, fixture?, path?}` cases as a regression suite |
| `run_record_diff(left, right)` | left, right | dict | Compare two run records and summarize stage/status deltas |
| `eval_suite_manifest(payload)` | payload: dict | dict | Normalize a grouped eval suite manifest |
| `eval_suite_run(manifest)` | manifest: dict | dict | Evaluate a manifest of saved runs, fixtures, and optional baselines |
| `eval_metric(name, value, metadata?)` | name: string, value: any, metadata: dict | nil | Record a named metric into the eval metric store |
| `eval_metrics()` | — | list | Return all recorded eval metrics as `{name, value, metadata?}` dicts |

`workflow_execute` options currently include:

- `max_steps`
- `persist_path`
- `resume_path`
- `resume_run`
- `replay_path`
- `replay_run`
- `replay_mode` (`"deterministic"` currently replays saved stage fixtures)
- `parent_run_id`
- `root_run_id`
- `execution` (`{cwd?, env?, worktree?}` for isolated delegated execution)
- `audit` (seed mutation-session metadata for trust/audit grouping)
- `mutation_scope`
- `approval_policy` (declarative tool approval policy; see below)

`verify` nodes may also define execution checks inside `node.verify`, including:

- `command` to execute via the host shell in the current execution context
- `assert_text` to require visible output to contain a substring
- `expect_status` to require a specific exit status

### Tool lifecycle hooks

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `register_tool_hook(config)` | config: dict | nil | Register a pre/post hook for tool calls matching `pattern` (glob). `deny` string blocks matching tools; `max_output` int truncates results |
| `clear_tool_hooks()` | none | nil | Remove all registered tool hooks |

### Context and compaction utilities

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `estimate_tokens(messages)` | messages: list | int | Estimate token count for a message list (chars / 4 heuristic) |
| `microcompact(text, max_chars?)` | text, max_chars (default 20000) | string | Snip oversized text, keeping head and tail with a marker |
| `select_artifacts_adaptive(artifacts, policy)` | artifacts: list, policy: dict | list | Deduplicate, microcompact oversized artifacts, then select with token budget |
| `transcript_auto_compact(messages, options?)` | messages: list, options: dict | list | Run the same transcript auto-compaction pipeline used by `agent_loop` |

### Delegated workers

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `spawn_agent(config)` | config: dict | dict | Start a worker from a workflow graph or delegated stage config |
| `sub_agent_run(task, options?)` | task: string, options: dict | dict | Run an isolated child agent loop and return a clean envelope `{summary, artifacts, evidence_added, tokens_used, budget_exceeded, ...}` without leaking the child transcript into the parent |
| `send_input(handle, task)` | handle, task | dict | Re-run a completed worker with a new task, carrying forward worker state where applicable |
| `resume_agent(id_or_snapshot_path)` | id or path | dict | Restore a persisted worker snapshot into the current runtime |
| `wait_agent(handle_or_list)` | handle or list | dict or list | Wait for one worker or a list of workers to finish |
| `close_agent(handle)` | handle | dict | Cancel a worker and mark it terminal |
| `list_agents()` | none | list | List worker summaries tracked by the current runtime |

`spawn_agent(...)` accepts either:

- `{task, graph, artifacts?, options?, name?, wait?}` for typed workflow runs
- `{task, node, artifacts?, transcript?, name?, wait?}` for delegated stage runs
- Either shape may also include `policy: <capability_policy>` to narrow the
  worker's inherited execution ceiling.
- Either shape may also include `tools: ["name", ...]` as shorthand for a
  worker policy that only allows those tool names.
- Either shape may also include `execution: {cwd?, env?, worktree?}` where
  `worktree` accepts `{repo, path?, branch?, base_ref?, cleanup?}`.
- Either shape may also include `audit: {session_id?, parent_session_id?, mutation_scope?, approval_policy?}`

Worker configs may also include `carry` to control continuation behavior:

- `carry: {artifacts: "inherit" | "none" | <context_policy>}`
- `carry: {resume_workflow?: bool, persist_state?: bool}`

To give a spawned worker prior conversation context, open a session
before spawning and set `model_policy.session_id` on the worker's node.
Use `agent_session_fork(parent)` if the worker should start from a
branch of an existing conversation; `agent_session_reset(id)` before
the call if you want a fresh run with the same id.

Workers return handle dicts with an `id`, lifecycle timestamps, `status`,
`mode`, result/error fields, transcript presence, produced artifact count,
snapshot/child-run paths, and `audit` mutation-session metadata when available.
When a worker-scoped policy denies a tool call, the agent receives a structured
tool result payload: `{error: "permission_denied", tool: "...", reason: "..."}`.

`sub_agent_run(task, options?)` is the lighter-weight context-firewall primitive.
It starts a child session, runs a full `agent_loop`, and returns only a single
typed envelope to the parent:

- `summary`, `artifacts`, `evidence_added`, `tokens_used`, `budget_exceeded`,
  `session_id`, and optional `data`
- `ok: false` plus `error: {category, message, tool?}` when the child fails or
  hits a capability denial
- `background: true` returns a normal worker handle whose `mode` is `sub_agent`

Options mirror `agent_loop` where relevant (`provider`, `model`, `tools`,
`tool_format`, `max_iterations`, `token_budget`, `policy`, `approval_policy`,
`session_id`, `system`) and also accept:

- `allowed_tools: ["name", ...]` to narrow the child tool registry and
  capability ceiling
- `returns: {schema: ...}` to validate the child summary as structured output

### Artifacts and context

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `artifact(payload)` | payload: dict | artifact | Normalize a typed artifact/resource |
| `artifact_derive(parent, kind, extra?)` | parent, kind, extra | artifact | Derive a new artifact from a prior one |
| `artifact_select(artifacts, policy?)` | artifacts, policy | list | Select artifacts under context policy and budget |
| `artifact_context(artifacts, policy?)` | artifacts, policy | string | Render selected artifacts into context |
| `artifact_workspace_file(path, content, extra?)` | path, content, extra | artifact | Build a normalized workspace-file artifact with path provenance |
| `artifact_workspace_snapshot(paths, summary?, extra?)` | paths, summary, extra | artifact | Build a workspace snapshot artifact for host/editor context |
| `artifact_editor_selection(path, text, extra?)` | path, text, extra | artifact | Build an editor-selection artifact from host UI state |
| `artifact_verification_result(title, text, extra?)` | title, text, extra | artifact | Build a verification-result artifact |
| `artifact_test_result(title, text, extra?)` | title, text, extra | artifact | Build a test-result artifact |
| `artifact_command_result(command, output, extra?)` | command, output, extra | artifact | Build a command-result artifact with structured output |
| `artifact_diff(path, before, after, extra?)` | path, before, after, extra | artifact | Build a unified diff artifact from before/after text |
| `artifact_git_diff(diff_text, extra?)` | diff_text, extra | artifact | Build a git-diff artifact from host/tool output |
| `artifact_diff_review(target, summary?, extra?)` | target, summary, extra | artifact | Build a diff-review artifact linked to a diff/patch target |
| `artifact_review_decision(target, decision, extra?)` | target, decision, extra | artifact | Build an accept/reject review-decision artifact linked by lineage |
| `artifact_patch_proposal(target, patch, extra?)` | target, patch, extra | artifact | Build a proposed patch artifact linked to an existing target |
| `artifact_verification_bundle(title, checks, extra?)` | title, checks, extra | artifact | Bundle structured verification checks into one review artifact |
| `artifact_apply_intent(target, intent, extra?)` | target, intent, extra | artifact | Record an apply or merge intent linked to a reviewed artifact |

Core artifact kinds commonly used by the runtime include `resource`,
`workspace_file`, `workspace_snapshot`, `editor_selection`, `summary`,
`transcript_summary`, `diff`, `git_diff`, `patch`, `patch_set`,
`patch_proposal`, `diff_review`, `review_decision`, `verification_bundle`,
`apply_intent`, `test_result`, `verification_result`, `command_result`,
and `plan`.

### Sessions

Sessions are the first-class resource for agent-loop conversations.
They own a transcript history, closure subscribers, and a lifecycle.
See the [Sessions](./sessions.md) chapter for the full model.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `agent_session_open(id?)` | id: string or nil | string | Idempotent open; `nil` mints a UUIDv7 |
| `agent_session_exists(id)` | id | bool | Safe on unknown ids |
| `agent_session_length(id)` | id | int | Message count; errors on unknown id |
| `agent_session_snapshot(id)` | id | dict or nil | Read-only deep copy of the transcript |
| `agent_session_reset(id)` | id | nil | Wipes history; preserves id and subscribers |
| `agent_session_fork(src, dst?)` | src, dst | string | Copies transcript; subscribers are not copied |
| `agent_session_trim(id, keep_last)` | id, keep_last: int | int | Retain last `keep_last` messages; returns kept count |
| `agent_session_compact(id, opts)` | id, opts: dict | int | Runs the LLM/truncate/observation-mask compactor |
| `agent_session_inject(id, message)` | id, message: dict | nil | Appends `{role, content, …}`; missing `role` errors |
| `agent_session_close(id)` | id | nil | Evicts immediately regardless of LRU cap |

Pair with `agent_loop(..., {session_id: id, ...})`: prior messages load
as prefix and the final transcript is persisted back on exit.

### Transcript lifecycle

Lower-level transcript primitives. Most callers should prefer sessions;
these remain useful for building synthetic transcripts, replay fixtures,
and offline analysis.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `transcript(metadata?)` | metadata: any | transcript | Create an empty transcript |
| `transcript_messages(transcript)` | transcript | list | Return transcript messages |
| `transcript_assets(transcript)` | transcript | list | Return transcript asset descriptors |
| `transcript_add_asset(transcript, asset)` | transcript, asset | transcript | Register a durable asset reference on a transcript |
| `transcript_events(transcript)` | transcript | list | Return canonical transcript events |
| `transcript_events_by_kind(transcript, kind)` | transcript, kind | list | Filter transcript events by their `kind` field |
| `transcript_stats(transcript)` | transcript | dict | Count messages, tool calls, and visible events on a transcript |
| `transcript_summary(transcript)` | transcript | string or nil | Return transcript summary |
| `transcript_fork(transcript, options?)` | transcript, options | transcript | Fork transcript state |
| `transcript_reset(options?)` | options | transcript | Start a fresh active transcript with optional metadata |
| `transcript_archive(transcript)` | transcript | transcript | Mark transcript archived and append an internal lifecycle event |
| `transcript_abandon(transcript)` | transcript | transcript | Mark transcript abandoned and append an internal lifecycle event |
| `transcript_resume(transcript)` | transcript | transcript | Mark transcript active again and append an internal lifecycle event |
| `transcript_compact(transcript, options?)` | transcript, options | transcript | Locally compact transcript messages |
| `transcript_summarize(transcript, options?)` | transcript, options | transcript | Compact via LLM-generated summary |
| `transcript_auto_compact(messages, options?)` | messages, options | list | Apply the agent-loop compaction pipeline to a message list |
| `transcript_render_visible(transcript)` | transcript | string | Render only public/human-visible messages |
| `transcript_render_full(transcript)` | transcript | string | Render the full execution history |

Transcript messages may now carry structured block content instead of plain
text. Use `add_user(...)`, `add_assistant(...)`, or `add_message(...)` with a
list of blocks such as `{type: "text", text: "..."}`,
`{type: "image", asset_id: "..."}`, `{type: "file", asset_id: "..."}`, and
`{type: "tool_call", ...}`, with per-block
`visibility: "public" | "internal" | "private"`. Durable media belongs in
`transcript.assets`, while message/event blocks should reference those assets
by id or path.
