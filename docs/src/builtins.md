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
| `json_pointer(value, ptr)` | value: any, ptr: string | value | Read an RFC 6901 JSON Pointer path. Returns `nil` when missing |
| `json_pointer_set(value, ptr, new)` | value: any, ptr: string, new: any | value | Return a copy with a JSON Pointer path replaced or inserted at an existing parent |
| `json_pointer_delete(value, ptr)` | value: any, ptr: string | value | Return a copy with a JSON Pointer path removed. Missing paths are unchanged |
| `jq(value, expr)` | value: any, expr: string | list | Evaluate a jq-like expression and return the emitted stream as a list |
| `jq_first(value, expr)` | value: any, expr: string | value | Return the first `jq` result, or `nil` when the expression emits nothing |

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

### JSON Pointer and jq-like queries

`json_pointer` implements RFC 6901 addressing, including `~0` for `~`
and `~1` for `/`. `json_pointer_set` and `json_pointer_delete` return
mutated copies instead of changing the input value in place. Setting a
dict key inserts or replaces it when the parent exists; setting a list
index replaces that element, and `-` appends.

`jq` supports the v1 scripting subset: identity, field access,
quoted-key access, array iteration/index/slice, pipes, commas,
`length`, `keys`, `values`, `type`, `map(...)`, `select(...)`,
`==`, `!=`, `<`, `>`, `and`, `or`, `not`, object construction, and
recursive descent. It always returns the emitted stream as a list;
`jq_first` is the convenience form for single-result queries.

```harn
let api = json_parse(response.body)
let email = json_pointer(api, "/users/0/email")
let active_emails = jq(api, ".users[] | select(.active == true) | .email")
let summary = jq_first(api, "{ count: .users | length, next: .meta.next }")
```

## Multipart forms

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `multipart_parse(body, content_type, options?)` | body: bytes or string, content_type: string, options: dict | dict | Parse a buffered `multipart/form-data` request body. Options support `max_total_bytes`, `max_field_bytes`, and `max_fields` |
| `multipart_field_bytes(field)` | field: dict | bytes | Return a parsed field's raw bytes |
| `multipart_field_text(field)` | field: dict | string | Decode a parsed field's bytes as UTF-8, throwing on invalid text |
| `multipart_form_data(fields, options?)` | fields: list, options: dict | dict | Deterministically build `{content_type, boundary, body}` test fixtures from field dicts |

`multipart_parse` returns `{boundary, fields, field_count, total_bytes}`.
Each field is `{name, filename, content_type, headers, bytes, text}`. `text`
is `nil` when the uploaded bytes are not valid UTF-8; use
`multipart_field_text(field)` when invalid UTF-8 should be an error.

```harn
let fixture = multipart_form_data([
  {name: "title", content: "Quarterly report"},
  {
    name: "upload",
    filename: "report.bin",
    content_type: "application/octet-stream",
    content: bytes_from_hex("000102ff"),
  },
])

let form = multipart_parse(fixture.body, fixture.content_type, {
  max_total_bytes: 1048576,
  max_field_bytes: 262144,
  max_fields: 8,
})

let title = multipart_field_text(form.fields[0])
let uploaded = multipart_field_bytes(form.fields[1])
println(title)
println(bytes_to_hex(uploaded))
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
| `rng_seed(seed)` | seed: int | rng | Create a reproducible RNG handle |
| `random()` | none | float | Random float in [0, 1) |
| `random(rng)` | rng: rng | float | Random float from a seeded RNG handle |
| `random_int(min, max)` | min: int, max: int | int | Random integer in [min, max] inclusive |
| `random_int(rng, min, max)` | rng: rng, min: int, max: int | int | Random integer from a seeded RNG handle |
| `random_choice(list)` | list: list | any or nil | Random element from a list, or nil for an empty list |
| `random_choice(rng, list)` | rng: rng, list: list | any or nil | Random element using a seeded RNG handle |
| `random_shuffle(list)` | list: list | list | Shuffled copy of a list |
| `random_shuffle(rng, list)` | rng: rng, list: list | list | Shuffled copy using a seeded RNG handle |
| `mean(items)` | items: list[number] | float | Arithmetic mean of a numeric list |
| `median(items)` | items: list[number] | float | Median of a numeric list |
| `variance(items, sample?)` | items: list[number], sample: bool | float | Population variance, or sample variance when `sample = true` |
| `stddev(items, sample?)` | items: list[number], sample: bool | float | Population standard deviation, or sample mode when `sample = true` |
| `percentile(items, p)` | items: list[number], p: 0..100 | float | R-7 percentile interpolation |

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
| `unicode_normalize(str, form)` | str: string, form: `"NFC"\|"NFD"\|"NFKC"\|"NFKD"` | string | Normalize Unicode into the requested form |
| `unicode_graphemes(str)` | str: string | list | Split a string into extended grapheme clusters |
| `str_pad(str, width, char?, side?)` | str: string, width: int, char: string, side: `"left"\|"right"\|"both"` | string | Pad to a grapheme width using the given fill character |
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

### Collection helper builtins

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `chunk(list, size)` | list: list, size: int | list | Split into chunks of size |
| `window(list, size, step?)` | list: list, size: int, step: int | list | Sliding windows with optional stride |
| `group_by(list, fn)` | list: list, fn: closure | dict | Group into a dict keyed by callback result |
| `partition(list, fn)` | list: list, fn: closure | dict | Split into `{match, no_match}` lists |
| `dedup_by(list, fn)` | list: list, fn: closure | list | Keep the first item for each callback-derived key |
| `flat_map(list, fn)` | list: list, fn: closure | list | Map then flatten one level |

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
| `path_workspace_info(path, workspace_root?)` | path: string, workspace_root?: string | dict | Classify a path as `workspace_relative`, `host_absolute`, or `invalid`, and project both workspace-relative and host-absolute forms when known |
| `path_workspace_normalize(path, workspace_root?)` | path: string, workspace_root?: string | string or nil | Normalize a path into workspace-relative form when it is safely inside the workspace (including common leading-slash drift like `/packages/...`) |

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
| `render_string(template, bindings?)` | template: string, bindings: dict | string | Render an inline template string with the same template engine as `render(...)`. Useful when a library wants to embed a template directly in source instead of shipping a separate `.prompt` file. `{{ include "..." }}` still resolves relative to the current module's asset root |

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
| `uuid_parse(str)` | str: string | string or nil | Parse and canonicalize a UUID string, or return nil if invalid |
| `uuid_v5(namespace, name)` | namespace: UUID or `"dns"\|"url"\|"oid"\|"x500"`, name: string | string | Generate a deterministic namespaced v5 UUID |
| `uuid_v7()` | none | string | Generate a time-ordered v7 UUID |
| `uuid_nil()` | none | string | Return the all-zero nil UUID |
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
| `regex_match(pattern, text, flags?)` | pattern: string, text: string, flags: string | list or nil | Find all non-overlapping matches. Optional flags: `i`, `m`, `s`, `x` |
| `regex_split(text, pattern, flags?)` | text: string, pattern: string, flags: string | list | Split text by regex matches |
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
| `base64url_encode(string)` | string: string | string | Base64 encode a string with the URL-safe alphabet and no padding |
| `base64url_decode(string)` | string: string | string | Decode a URL-safe base64 string without padding. Throws on invalid input |
| `base32_encode(string)` | string: string | string | Base32 encode a string using the RFC 4648 alphabet with padding |
| `base32_decode(string)` | string: string | string | Decode a base32 string. Throws on invalid input |
| `hex_encode(string)` | string: string | string | Hex encode a string as lowercase ASCII |
| `hex_decode(string)` | string: string | string | Decode a hex string. Throws on invalid input |
| `url_encode(string)` | string: string | string | URL percent-encode a string. Unreserved characters (alphanumeric, `-`, `_`, `.`, `~`) pass through unchanged |
| `url_decode(string)` | string: string | string | Decode a URL-encoded string. Decodes `%XX` sequences and `+` as space |

Example:

```harn
let encoded = base64_encode("Hello, World!")
println(encoded)                  // SGVsbG8sIFdvcmxkIQ==
println(base64_decode(encoded))   // Hello, World!
```

```harn
println(base64url_encode(">>>???///"))     // Pj4-Pz8_Ly8v
println(base32_encode("foobar"))           // MZXW6YTBOI======
println(hex_encode("hello"))               // 68656c6c6f
println(hex_decode("68656c6c6f"))          // hello
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

### HMAC and signature comparison

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `hmac_sha256(key, message)` | key: string, message: string | string | HMAC-SHA256 as a lowercase hex-encoded string. Most webhook providers (GitHub, Stripe) send signatures in this form |
| `hmac_sha256_base64(key, message)` | key: string, message: string | string | HMAC-SHA256 as standard base64 (used by Slack-style signatures) |
| `constant_time_eq(a, b)` | a: string, b: string | bool | Timing-safe string equality. Always use this to compare HMAC signatures — plain `==` can leak the signature byte-by-byte through timing differences |
| `signed_url(base, claims, secret, expires_at, options?)` | base: string, claims: dict, secret: string, expires_at: int, options: dict | string | Create a short-lived HMAC-SHA256 signed absolute URL or absolute path. The signature is URL-safe base64 without padding |
| `verify_signed_url(url, secret_or_keys, now, options?)` | url: string, secret_or_keys: string or dict, now: int, options: dict | dict | Verify a signed URL/path with constant-time signature comparison and optional clock skew. Returns `{valid, reason, signature_valid, expired, expires_at, kid, claims}` |
| `jwt_sign(alg, claims, private_key)` | alg: string, claims: dict, private_key: string | string | Sign a compact JWT/JWS using a PEM private key. Supports `ES256` with P-256 EC private keys and `RS256` with RSA private keys |

Example (GitHub-style webhook signature verification):

```harn
let signature = "sha256=" + hmac_sha256(secret, raw_body)
if !constant_time_eq(signature, request_signature) {
  throw "invalid signature"
}
```

Example (short-lived receipt or artifact link):

```harn
let expires_at = timestamp() + 300
let link = signed_url(
  "https://portal.example.test/receipts/r_123",
  {artifact: "transcript.json"},
  receipt_secret,
  expires_at,
  {kid: "v2"},
)

let verified = verify_signed_url(
  link,
  {v1: old_receipt_secret, v2: receipt_secret},
  timestamp(),
  {skew_seconds: 30},
)
if !verified.valid {
  throw "invalid receipt link: " + verified.reason
}
```

`signed_url` accepts either an absolute URL with a host or an absolute path
beginning with `/`. Existing query parameters and `claims` are merged, reserved
parameters are then added, and the query is canonicalized by percent-encoding
each key/value with RFC 3986 unreserved characters left plain and sorting
encoded pairs lexicographically. Paths preserve `/`, preserve existing `%XX`
escapes with uppercase hex, and percent-encode other non-unreserved bytes. The
signed payload is the version marker, canonical resource (origin + path for
URLs, path for paths), and canonical query without the signature. Default
parameter names are `exp`, `kid`, and `sig`; override them with `expires_param`,
`kid_param`, and `signature_param` in `options`. `kid` is optional when signing;
verification can use either one secret string or a dict mapping key ids to
secrets.

JWT signing expects `claims` to be a JSON object. The private key must be PEM encoded:
`ES256` accepts PKCS#8 EC private keys such as `-----BEGIN PRIVATE KEY-----`;
`RS256` accepts RSA private keys such as `-----BEGIN RSA PRIVATE KEY-----` or
PKCS#8 private keys. Invalid algorithms, non-dict claims, and malformed PEM
keys throw runtime errors.

```harn
let token = jwt_sign(
  "ES256",
  {iss: app_id, iat: timestamp(), exp: timestamp() + 600},
  read_file("github-app-private-key.pem"),
)
```

### Cookies and sessions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `cookie_parse(headers)` | headers: string, list, or dict | dict | Parse request `Cookie` header values into `{cookies, pairs, duplicates, invalid}`. `cookies` keeps the first value for each name; `duplicates` records all values for repeated names |
| `cookie_serialize(name, value, options?)` | name: string, value: string, options: dict | string | Serialize one `Set-Cookie` value. Options support `http_only`, `secure`, `same_site`, `path`, `domain`, `max_age`, and `expires` |
| `cookie_delete(name, options?)` | name: string, options: dict | string | Serialize a deletion cookie with `Max-Age=0` and a Unix epoch `Expires`; secure session defaults are applied unless overridden |
| `cookie_sign(value, secret)` | value: string, secret: string | string | Return `value.signature` using HMAC-SHA256 and URL-safe base64 for tamper-evident cookie values |
| `cookie_verify(signed_value, secret)` | signed_value: string, secret: string | dict | Verify a signed cookie value and return `{ok, value, error}` without throwing on signature failure |
| `session_sign(payload, secret)` | payload: any JSON value, secret: string | string | Return a stateless signed session token containing the JSON payload |
| `session_verify(token, secret)` | token: string, secret: string | dict | Verify a stateless session token and return `{ok, payload, error}` without throwing on signature failure |
| `session_cookie(name, payload, secret, options?)` | name: string, payload: any JSON value, secret: string, options: dict | string | Serialize a signed session cookie. Defaults are `Path=/`, `HttpOnly`, `Secure`, and `SameSite=Lax` |
| `session_from_cookies(headers, name, secret)` | headers: string/list/dict, name: string, secret: string | dict | Parse request cookies, read `name`, and verify it as a stateless session token |
| `cookie_round_trip(request_cookie?, set_cookie)` | request_cookie: string/list/dict, set_cookie: string/list/dict | dict | Test helper that applies response `Set-Cookie` headers to an existing request cookie header and returns `{cookie_header, cookies}` for the next request |

`cookie_parse` accepts a raw `Cookie` string, a list of strings, or a headers
dict containing `Cookie`/`cookie`. Empty segments are ignored. Invalid segments
are skipped and reported in `invalid`. When the same cookie name appears more
than once, `cookies[name]` keeps the first value and `duplicates[name]` contains
all observed values in wire order.

```harn
let parsed = cookie_parse("sid=abc; theme=light; sid=old")
println(parsed.cookies.sid)       // abc
println(parsed.duplicates.sid[1]) // old
```

`cookie_serialize` validates names and values before writing a `Set-Cookie`
header. `SameSite=None` requires `Secure` so insecure cross-site cookies are
rejected early.

```harn
let header = cookie_serialize("theme", "dark", {
  path: "/",
  max_age: 3600,
  http_only: true,
  secure: true,
  same_site: "Strict",
})
```

`session_*` helpers are stateless: all trusted session data lives inside the
signed cookie token. For store-backed sessions, put only an opaque session ID in
the cookie and store the mutable server-side state with `store_*`,
`shared_map_*`, or an application database.

```harn
let set_cookie = session_cookie("harn_session", {user: "alice"}, secret)
let next_request = cookie_round_trip(set_cookie)
let session = session_from_cookies(next_request.cookie_header, "harn_session", secret)
if !session.ok {
  throw "invalid session"
}
```

## Date/Time

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `date_now()` | none | dict | Current UTC datetime as dict with `year`, `month`, `day`, `hour`, `minute`, `second`, `weekday`, `timestamp`, and `iso8601` fields |
| `date_now_iso()` | none | string | Current UTC datetime as RFC 3339 / ISO 8601 string |
| `date_parse(str)` | str: string | int or float | Parse RFC 3339 / ISO 8601 strings (including offsets and fractional seconds) into a Unix timestamp. Falls back to legacy numeric-component extraction for malformed legacy inputs, but validates the resulting calendar date |
| `date_format(dt, format?, tz?)` | dt: float, int, or dict; format: string (default `"%Y-%m-%d %H:%M:%S"`); tz: IANA timezone string | string | Format a timestamp or date dict using chrono/strftime format codes such as `%Y`, `%m`, `%d`, `%H`, `%M`, `%S`, `%A`, `%B`, `%Z`, `%z`, `%:z`, `%f`, `%3f`, and `%s`. Negative pre-epoch timestamps are supported |
| `date_in_zone(dt, tz)` | dt: float, int, or dict; tz: IANA timezone string | dict | Convert a timestamp into timezone-local fields: `year`, `month`, `day`, `hour`, `minute`, `second`, `weekday`, `zone`, `offset_seconds`, `timestamp`, and `iso8601` |
| `date_to_zone(dt, tz)` | dt: float, int, or dict; tz: IANA timezone string | string | Convert a timestamp to an RFC 3339 string with the timezone's offset |
| `date_from_components(parts, tz?)` | parts: dict; tz: IANA timezone string (default UTC) | int or float | Build a Unix timestamp from `{year, month, day, hour?, minute?, second?}` interpreted in the given timezone |
| `date_add(dt, duration)` | dt: float, int, or dict; duration: duration | int or float | Add a duration to a timestamp |
| `date_diff(a, b)` | a, b: float, int, or dict | duration | Return the signed duration `a - b` |
| `duration_ms(n)` | n: number | duration | Create a duration from milliseconds |
| `duration_seconds(n)` | n: number | duration | Create a duration from seconds |
| `duration_minutes(n)` | n: number | duration | Create a duration from minutes |
| `duration_hours(n)` | n: number | duration | Create a duration from hours |
| `duration_days(n)` | n: number | duration | Create a duration from days |
| `duration_to_seconds(duration)` | duration: duration | int | Convert a duration to whole seconds |
| `duration_to_human(duration)` | duration: duration | string | Format a compact duration such as `"3h 14m"` |
| `weekday_name(dt, tz?)` | dt: float, int, or dict; tz: IANA timezone string | string | Weekday name for a timestamp, optionally in a timezone |
| `month_name(dt, tz?)` | dt: float, int, or dict; tz: IANA timezone string | string | Month name for a timestamp, optionally in a timezone |

Migration note: `date_parse` now tries standards-compliant RFC 3339 / ISO 8601 parsing first.
Malformed strings that previously happened to work through digit extraction still fall back to
that behavior, but impossible calendar dates such as `"2024-02-31"` now throw instead of rolling
through timestamp arithmetic.

## Vision

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `vision_ocr(image, options?)` | image: string or dict, options: dict | dict | Run deterministic OCR over an image and return `StructuredText` with `text`, `blocks`, `lines`, `tokens`, `source`, `backend`, and `stats`. `image` may be a path string, `{path, ...}`, `{storage: {path}, ...}`, `{bytes_base64, mime_type, name?}`, or `{data_url, name?}`. `options.language` sets the Tesseract language code when the default backend is in use |

Example:

```harn
import "std/vision"

let text = ocr("fixtures/receipt.png", {language: "eng"})
println(text.text)
println(text.tokens[0]?.text)
println(text.source.sha256)
```

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
| `http_download(url, dst_path, options?)` | url: string, dst_path: string, options: dict | dict | Stream a response body to a file |
| `egress_policy(config)` | config: dict | dict | Install the process egress policy used by HTTP, SSE, WebSocket, and connector outbound calls |
| `http_server_tls_plain()` | none | dict | Build HTTP-server TLS config for intentional cleartext/local listener mode |
| `http_server_tls_edge(options?)` | options: dict | dict | Build HTTP-server TLS config for edge-terminated HTTPS; local listener stays plain and HSTS is enabled by default |
| `http_server_tls_pem(cert_path, key_path)` | cert_path: string, key_path: string | dict | Build in-process HTTPS config from PEM files; missing files throw before startup |
| `http_server_tls_self_signed_dev(hosts?)` | hosts: string or list | dict | Generate a self-signed development cert/key config for local HTTPS testing. HSTS is disabled |
| `http_server_security_headers(tls_config)` | tls_config: dict | dict | Return TLS-aware response headers such as `strict-transport-security`; edge and PEM modes enable HSTS by default, plain and self-signed dev do not |
| `http_session(options?)` | options: dict | string | Create a reusable host-managed HTTP client/session handle |
| `http_session_request(session, method, url, options?)` | session: string, method: string, url: string, options: dict | dict | Run an HTTP request through a reusable session |
| `http_session_close(session)` | session: string | bool | Close a reusable HTTP session handle |
| `http_stream_open(url, options?)` | url: string, options: dict | string | Open a streaming HTTP response handle |
| `http_stream_read(stream, max_bytes?)` | stream: string, max_bytes: int | bytes or nil | Read the next response chunk |
| `http_stream_info(stream)` | stream: string | dict | Return `{status, headers, ok}` for an open stream |
| `http_stream_close(stream)` | stream: string | bool | Close a streaming HTTP response handle |
| `sse_connect(method, url, options?)` | method: string, url: string, options: dict | string | Open an SSE/Streamable HTTP receive handle |
| `sse_receive(stream, timeout_ms?)` | stream: string, timeout_ms: int | dict or nil | Receive one SSE event with timeout/backpressure |
| `sse_close(stream)` | stream: string | bool | Close an SSE handle |
| `sse_event(event, options?)` | event: any, options: dict | string | Format a server-sent event frame |
| `sse_server_response(options?)` | options: dict | dict | Create a `text/event-stream` response handle |
| `sse_server_send(stream, event, options?)` | stream: string or dict, event: any, options: dict | bool | Write one event frame to a server SSE response |
| `sse_server_heartbeat(stream, comment?)` | stream: string or dict, comment: string | bool | Write an SSE comment/heartbeat frame |
| `sse_server_flush(stream)` | stream: string or dict | bool | Flush pending server SSE frames when the client is still connected |
| `sse_server_status(stream)` | stream: string or dict | dict | Inspect buffered event count, close, cancel, and disconnect state |
| `sse_server_disconnected(stream)` | stream: string or dict | bool | Return whether the mock/client side disconnected |
| `sse_server_cancelled(stream)` | stream: string or dict | bool | Return whether the response was cancelled |
| `sse_server_cancel(stream, reason?)` | stream: string or dict, reason: string | bool | Mark the response cancelled and closed |
| `sse_server_close(stream)` | stream: string or dict | bool | Close a server SSE response |
| `sse_server_mock_receive(stream)` | stream: string or dict | dict | Deterministically read the next buffered server SSE frame in tests |
| `sse_server_mock_disconnect(stream)` | stream: string or dict | bool | Simulate a client disconnecting from a server SSE response |
| `websocket_connect(url, options?)` | url: string, options: dict | string | Open a WebSocket client handle |
| `websocket_server(bind?, options?)` | bind: string, options: dict | dict | Start a host-managed WebSocket server and return `{id, addr, url}` |
| `websocket_route(server, path, options?)` | server: string or dict, path: string, options: dict | bool | Register an HTTP upgrade route on a WebSocket server |
| `websocket_accept(server, timeout_ms?)` | server: string or dict, timeout_ms: int | dict or nil | Accept one upgraded connection and return its socket handle plus peer metadata |
| `websocket_send(socket, message, options?)` | socket: string, message: string or bytes, options: dict | bool | Send a WebSocket text/binary/ping/pong/close message |
| `websocket_receive(socket, timeout_ms?)` | socket: string, timeout_ms: int | dict or nil | Receive one WebSocket message with timeout/backpressure |
| `websocket_close(socket)` | socket: string | bool | Close a WebSocket handle |
| `http_server(options?)` | options: dict | dict | Create an in-process inbound HTTP server definition for host adapters or synthetic tests |
| `http_server_route(server, method, path_template, handler, options?)` | server: dict/string, method: string, path_template: string, handler: closure, options: dict | dict | Register a route. Templates support `{name}` and `:name` path params |
| `http_server_before(server, handler)` | server: dict/string, handler: closure | dict | Register before middleware. Return a request to continue or a response dict to short-circuit |
| `http_server_after(server, handler)` | server: dict/string, handler: closure | dict | Register after middleware. Receives `(response, request)` and may return a replacement response |
| `http_server_request(server, request)` | server: dict/string, request: dict | dict | Dispatch a synthetic or host-adapted request through the server |
| `http_server_test(server, request)` | server: dict/string, request: dict | dict | Alias for `http_server_request`, intended for script-level tests |
| `http_server_set_ready(server, ready)` | server: dict/string, ready: bool | bool | Set the server readiness gate used by request dispatch |
| `http_server_readiness(server, handler)` | server: dict/string, handler: closure | dict | Register a readiness callback for `http_server_ready` |
| `http_server_ready(server)` | server: dict/string | bool | Return readiness, invoking the readiness callback when present |
| `http_server_on_shutdown(server, handler)` | server: dict/string, handler: closure | dict | Register a shutdown lifecycle callback |
| `http_server_shutdown(server)` | server: dict/string | bool | Mark the server shut down and run shutdown callbacks |
| `http_response(status, body?, headers?)` | status: int, body: any, headers: dict | dict | Build a response dict |
| `http_response_text(text, options?)` | text: any, options: dict | dict | Build a text response. Options include `status` and `headers` |
| `http_response_json(value, options?)` | value: any, options: dict | dict | Build a JSON response with a JSON content type |
| `http_response_bytes(bytes, options?)` | bytes: bytes/string, options: dict | dict | Build a bytes response |
| `http_header(headers_or_message, name)` | headers/request/response: dict, name: string | string or nil | Read a header case-insensitively from a header dict, request, or response |
| `websocket_server_close(server)` | server: string or dict | bool | Stop a WebSocket server handle |

`http_get/post/put/patch/delete/request/session_request` return
`{status: int, headers: dict, body: string, ok: bool}`.
`http_download` returns `{bytes_written, status, headers, ok}`.
Options: `timeout_ms` (alias `timeout`, both in ms), `total_timeout_ms`,
`connect_timeout_ms`, `read_timeout_ms`, `retry: {max, backoff_ms}`,
legacy aliases `retries` / `backoff`, optional `retry_on` (status list),
optional `retry_methods` (defaults to `GET`, `HEAD`, `PUT`, `DELETE`,
`OPTIONS`), `headers` (dict), `auth` (string or `{bearer: "token"}` or
`{basic: {user, password}}`), `follow_redirects` (bool),
`max_redirects` (int), `body` (string), `multipart`
(`list<{name, value|value_base64|path, filename?, content_type?}>`),
`proxy` (string or `{url, no_proxy?}`), `proxy_auth` (`{user, pass}`),
`tls` (`{ca_bundle_path?, client_cert_path?, client_key_path?, client_identity_path?, pinned_sha256?}`),
and `decompress` (bool, default `true`). `timeout_ms` and `total_timeout_ms`
apply per attempt.
Retryable responses default to `408`, `429`, `500`, `502`, `503`, and
`504`; `Retry-After` is honored on `429` and `503` when retries are
enabled. Throws on network errors. `http_request(..., {session: handle})`
routes through an existing session when one is provided. `http_post`,
`http_put`, and `http_patch` accept an options dict as the second argument
when you want to send multipart without a separate string body.

`egress_policy({allow, deny, default})` installs a process-scoped outbound
network policy before user code opens real connections. Rules accept exact
hosts (`api.example.com`), suffix wildcards (`*.example.com`), IP literals or
CIDR ranges (`127.0.0.0/8`), and optional port restrictions
(`api.example.com:443`). Deny rules override allow rules; `default: "deny"`
turns the policy into an allowlist. Operators can seed the same policy without
editing scripts via comma-separated `HARN_EGRESS_ALLOW`, `HARN_EGRESS_DENY`,
and `HARN_EGRESS_DEFAULT=deny`.

```harn
pipeline main(task) {
  egress_policy({
    allow: ["api.example.com:443", "*.trusted.example", "10.0.0.0/8"],
    deny: ["blocked.trusted.example"],
    default: "deny",
  })

  let response = http_get("https://api.example.com/v1/status")
  println(response.status)
}
```

Blocked attempts throw `{type: "EgressBlocked", category:
"egress_blocked", host, port, reason, url}` and append an
`egress.blocked` event to `connectors.egress.audit` when an event log is
active. The same policy is checked by `http_request` and friends,
`http_session_request`, `http_stream_open`, `http_download`, `sse_connect`,
`websocket_connect`, and Rust-backed `connector_call` clients.

`http_stream_open` uses the same request options as `http_request`. The
returned handle can be inspected with `http_stream_info`, drained with
repeated `http_stream_read(stream, max_bytes)`, and closed explicitly with
`http_stream_close`. Reads return `bytes`; once the stream is exhausted they
return `nil`.

HTTP server TLS helper builtins only describe listener/security policy. Runtime
hosts such as `harn serve` consume the same modes: `plain` for deliberate
cleartext, `edge` when a proxy/load balancer terminates public TLS, `pem` for
in-process HTTPS with certificate/key files, and `self_signed_dev` for local
HTTPS testing. `http_server_security_headers(...)` emits HSTS for edge and PEM
configs so edge-terminated deployments can still set browser-facing security
headers from the Harn layer; it deliberately omits HSTS for plain and
self-signed dev configs.

Transport handles are strings owned by the VM host. Rust keeps responsibility
for TCP/TLS/socket lifecycle, HTTP pooling, HTTP-to-WebSocket upgrade handling,
SSE/WebSocket protocol parsing, backpressure, receive timeouts, cancellation by
dropping/closing handles, and resource limits. Connector packages should use
`sse_receive`, `websocket_accept`, and `websocket_receive` as pull-based loops;
each call reads at most one event/message and returns `{type: "timeout"}` on
timeout or `nil` after close.

SSE events return `{type: "open"}` or `{type: "event", event, data, id,
retry_ms}`. WebSocket receives return `{type: "text", data}`, `{type:
"binary", data_base64}`, `{type: "ping", data_base64}`, `{type: "pong",
data_base64}`, `{type: "close", code?, reason?}`, or `{type: "timeout"}`.
Options include `max_events`/`max_messages` and `max_message_bytes`. WebSocket
server route options also include `auth: {bearer: "token"}` and
`idle_timeout_ms`; unauthorized or unregistered upgrade paths are rejected
during the HTTP upgrade. Server outbound backpressure is explicit:
`send_buffer_messages` bounds queued server-to-client frames, and
`websocket_send` throws when that queue is full. `websocket_connect` accepts
`headers` and `auth: {bearer: "token"}` options for clients that need upgrade
metadata.

Minimal inbound echo:

```harn
pipeline websocket_echo() {
  let server = websocket_server("127.0.0.1:8787", {})
  websocket_route(server, "/acp", {auth: {bearer: env("ACP_TOKEN")}})

  while true {
    let conn = websocket_accept(server, 30000)
    if conn?.type == "timeout" {
      continue
    }

    let frame = websocket_receive(conn, 30000)
    if frame?.type == "text" {
      websocket_send(conn, frame.data, {})
    }
    websocket_close(conn)
  }
}
```

### Inbound HTTP server primitives

The server builtins define a Harn-native request router without binding a
socket themselves. A host adapter can translate real HTTP requests into
`http_server_request(...)`; tests can use the same path with
`http_server_test(...)`.

Requests passed to route handlers include:

- `method`, `path`, `path_params`/`params`, `query`, and normalized lowercase
  `headers`
- `body` as text plus `raw_body` bytes when retained
- `body_bytes`, `remote_addr`, and `client_ip`

`http_server({max_body_bytes, retain_raw_body, ready})` sets defaults.
Routes can override `max_body_bytes` and `retain_raw_body`. Body-limit
rejections return status `413` before middleware or handlers run.

Minimal webhook example:

```harn
pipeline default() {
  let server = http_server({max_body_bytes: 1048576, retain_raw_body: true})

  http_server_before(server, { req ->
    if http_header(req, "origin") != nil {
      return http_response_text("browser origins are rejected", {status: 403})
    }
    req
  })

  http_server_after(server, { response, _req ->
    response + {
      headers: response.headers + {
        ["strict-transport-security"]: "max-age=31536000",
      },
    }
  })

  http_server_route(server, "POST", "/hooks/{tenant}/{trigger}", { req ->
    let signature = http_header(req, "x-hub-signature-256")
    let expected = "sha256=" + hmac_sha256(secret_get("github/webhook-secret"), req.body)
    if signature != expected {
      return http_response_text("invalid signature", {status: 401})
    }

    let payload = json_parse(req.body)
    trigger_fire("github-webhook", {
      tenant: req.path_params.tenant,
      trigger: req.path_params.trigger,
      payload: payload,
      raw_body: req.raw_body,
      client_ip: req.client_ip,
    })
    http_response_json({accepted: true}, {status: 202, headers: {["retry-after"]: "0"}})
  })

  let probe = http_server_test(server, {
    method: "POST",
    path: "/hooks/acme/push",
    headers: {["x-hub-signature-256"]: "sha256=..."},
    body: "{\"ok\":true}",
    client_ip: "203.0.113.10",
  })
  println(probe.status)
}
```

### Server-side SSE primitives

Server-side SSE responses are VM-owned handles. `sse_server_response()` returns
`{id, type: "sse_response", status, headers, body: nil, streaming: true}` with
`content-type: text/event-stream; charset=utf-8`, `cache-control: no-cache`,
`connection: keep-alive`, and `x-accel-buffering: no` unless overridden.
`sse_server_send()` formats fields as UTF-8 SSE lines: `event`, `id`, `retry`
or `retry_ms`, and multi-line `data`. `sse_server_heartbeat()` writes comment
frames. `max_event_bytes` rejects oversized frames before buffering, and
`max_buffered_events` rejects writes when the client is not draining quickly
enough. `sse_server_flush()` reports whether the stream is still writable after
marking currently buffered events flushed. Writes return `false` after close,
cancel, or disconnect. Use `sse_server_status()`, `sse_server_disconnected()`,
and `sse_server_cancelled()` to observe shutdown state.

```harn
pipeline progress_stream(task) {
  let stream = sse_server_response({max_event_bytes: 4096})
  sse_server_send(stream, {event: "progress", id: "1", data: "queued"})
  sse_server_heartbeat(stream, "still working")
  sse_server_send(stream, {event: "progress", id: "2", data: "done"})
  sse_server_flush(stream)
  return stream
}
```

### Mock HTTP

For testing pipelines that make HTTP calls without hitting real servers.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `http_mock(method, url_pattern, response)` | method: string, url_pattern: string, response: dict | nil | Register a mock. Use `*` in url_pattern for glob matching (supports multiple `*` wildcards, e.g., `https://api.example.com/*/items/*`). `response` may be a single `{status, body, headers}` dict or `{responses: [...]}` to script retries. |
| `http_mock_clear()` | none | nil | Clear all mocks and recorded calls |
| `http_mock_calls()` | none | list | Return list of `{method, url, headers, body}` for all intercepted calls |
| `sse_mock(url_pattern, events_or_config)` | url_pattern: string, events_or_config: list or dict | nil | Register an in-process SSE stream mock. Events may be strings or `{event, data, id?, retry_ms?}` dicts. |
| `websocket_mock(url_pattern, messages_or_config)` | url_pattern: string, messages_or_config: list or dict | nil | Register an in-process WebSocket mock. Messages may be strings/bytes or `{type, data}` dicts; `{messages: [...], echo: true}` enables echoing sends. |
| `transport_mock_calls()` | none | list | Return recorded mocked SSE/WebSocket connect/send/close calls |
| `transport_mock_clear()` | none | nil | Clear mocked SSE/WebSocket transports and recorded calls |

```harn
http_mock("GET", "https://api.example.com/users", {
  responses: [
    {status: 429, headers: {"retry-after": "0"}},
    {status: 200, body: "{\"users\": [\"alice\"]}", headers: {}},
  ]
})
let resp = http_get("https://api.example.com/users", {
  retry: {max: 1, backoff_ms: 0}
})
assert_eq(resp.status, 200)
```

```harn
let stream = http_stream_open("https://example.com/archive.tar.gz", {
  decompress: false,
  connect_timeout_ms: 5000,
  read_timeout_ms: 30000,
})
let meta = http_stream_info(stream)
let chunk = http_stream_read(stream, 65536)
http_stream_close(stream)
```

## Postgres

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `pg_pool(source, options?)` | source: string or dict, options: dict | dict | Open a pooled Postgres connection |
| `pg_connect(source, options?)` | source: string or dict, options: dict | dict | Open a single-connection Postgres pool |
| `pg_query(handle, sql, params?)` | handle: dict, sql: string, params: list | list | Run a parameterized query and return decoded rows |
| `pg_query_one(handle, sql, params?)` | handle: dict, sql: string, params: list | dict or nil | Return the first decoded row, or nil when no row matches |
| `pg_execute(handle, sql, params?)` | handle: dict, sql: string, params: list | dict | Execute a parameterized statement and return `{rows_affected}` |
| `pg_transaction(pool, callback, options?)` | pool: dict, callback: closure, options: dict | any | Run a closure with a transaction handle, commit on success, rollback on throw |
| `pg_close(pool)` | pool: dict | bool | Close and unregister a pool |
| `pg_mock_pool(fixtures)` | fixtures: list | dict | Create a fixture-backed Postgres handle for tests |
| `pg_mock_calls(mock)` | mock: dict | list | Return recorded mock SQL calls |

Connection sources may be raw Postgres URLs, `env:NAME`, `secret:namespace/name`,
or `{url}`, `{env}`, or `{secret}` dictionaries. Pool options include
`max_connections`, `min_connections`, `acquire_timeout_ms`, `idle_timeout_ms`,
`max_lifetime_ms`, `ssl_mode`, `application_name`, and
`statement_cache_capacity`.

Use `params` for every dynamic value:

```harn
let rows = pg_query(
  db,
  "select id, payload from receipts where tenant_id = $1 and id = $2::uuid",
  [tenant_id, receipt_id],
)
```

Rows decode into dictionaries. JSON/JSONB becomes Harn values; UUID, date,
time, timestamp, and timestamptz decode as strings. Transaction `options` may
include `settings`, which are applied with transaction-local `set_config` for
RLS policies:

```harn
pg_transaction(db, { tx ->
  pg_execute(tx, "insert into event_log(tenant_id, kind) values ($1, $2)", [
    tenant_id,
    "receipt.created",
  ])
}, {settings: {"app.current_tenant_id": tenant_id}})
```

See [Postgres](./postgres.md) for the full persistence guide and mock fixture
examples.

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
| `host_tool_list()` | — | list | List host-exposed bridge tools as `{name, description, schema, deprecated}` |
| `host_tool_call(name, args)` | name: string, args: any | any | Invoke a bridge-exposed host tool by name using the existing `builtin_call` path |
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

`host_tool_list()` is the discovery surface for host-native tools such as
`Read`, `Edit`, `Bash`, or IDE actions exposed by the active bridge host.
Without a bridge it returns `[]`. `host_tool_call(name, args)` uses that same
bridge host's existing dynamic builtin dispatch path, so scripts can discover a
tool at runtime and then call it by name without hard-coding it into the
initial prompt. Import `std/host` when you want small helpers such as
`host_tool_lookup(name)` or `host_tool_available(name)`.

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
| `channel_select(channels, timeout?)` | channels: list[channel], timeout: int or duration | dict or nil | Select over a channel list with an optional timeout |

### Supervisors

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `supervisor_start(spec)` | spec: dict | supervisor handle | Start a named supervisor with child `task` closures, child kinds, restart policy, and propagation strategy |
| `supervisor_state(handle_or_id)` | handle or string | dict | Return supervisor children, status, restart counts, last errors, wait reasons, active leases, next restart times, and metrics |
| `supervisor_events(handle_or_id)` | handle or string | list | Return lifecycle events for started, stopped, failed, restarted, suppressed, escalated, and shutdown activity |
| `supervisor_metrics(handle_or_id)` | handle or string | dict | Return aggregate lifecycle counters |
| `supervisor_stop(handle_or_id, timeout?)` | handle or string, duration | dict | Request cooperative child cancellation, wait for drain, then force-abort remaining children |

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
| `llm_call_safe(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Non-throwing envelope around `llm_call`. Returns `{ok: bool, response: dict or nil, error: {category, message} or nil}`. `error.category` is one of `ErrorCategory`'s canonical strings (`"rate_limit"`, `"timeout"`, `"overloaded"`, `"server_error"`, `"transient_network"`, `"schema_validation"`, `"auth"`, `"not_found"`, `"circuit_open"`, `"tool_error"`, `"tool_rejected"`, `"egress_blocked"`, `"cancelled"`, `"generic"`) |
| `with_rate_limit(provider, fn, options?)` | provider: string, fn: closure, options: dict | whatever `fn` returns | Acquire a permit from the provider's sliding-window rate limiter, invoke `fn`, and retry with exponential backoff on retryable errors (`rate_limit`, `overloaded`, `transient_network`, `timeout`). Options: `max_retries` (default 5), `backoff_ms` (default 1000, capped at 30s after doubling) |
| `llm_completion(prefix, suffix?, system?, options?)` | prefix: string, suffix: string, system: string, options: dict | dict | Text completion / fill-in-the-middle request. Returns `{text, model, input_tokens, output_tokens}` |
| `agent_loop(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Multi-turn agent loop with `##DONE##` completion sentinel (`<done>##DONE##</done>` in tagged text-tool stages), daemon/idling support, and optional per-turn context filtering. Returns `{status, text, visible_text, llm: {iterations, duration_ms, input_tokens, output_tokens}, tools: {calls, successful, rejected, mode}, transcript, task_ledger, trace, …}` |
| `daemon_spawn(config)` | config: dict | dict | Start a daemon-mode agent and return a daemon handle with persisted state + queue metadata |
| `daemon_trigger(handle, event)` | handle: dict or string, event: any | dict | Enqueue a durable FIFO trigger event for a running daemon; throws `VmError::DaemonQueueFull` on overflow |
| `daemon_snapshot(handle)` | handle: dict or string | dict | Return the latest daemon snapshot plus live queue state (`pending_events`, `inflight_event`, counts, capacity) |
| `daemon_stop(handle)` | handle: dict or string | dict | Stop a daemon and preserve queued trigger state for resume |
| `daemon_resume(path)` | path: string | dict | Resume a daemon from its persisted state directory |
| `trigger_list()` | — | list | Return the live trigger registry snapshot as `list<TriggerBinding>` |
| `trigger_register(config)` | config: dict | dict | Dynamically register a trigger and return its `TriggerHandle` |
| `trigger_fire(handle, event)` | handle: dict or string, event: dict | dict | Fire a synthetic event into a trigger and return a `DispatchHandle`; execution routes through the trigger dispatcher |
| `trigger_replay(event_id)` | event_id: string | dict | Fetch a historical event from `triggers.events`, re-dispatch it through the trigger dispatcher, and thread `replay_of_event_id` through the returned `DispatchHandle` |
| `trigger_inspect_dlq()` | — | list | Return the current DLQ snapshot as `list<DlqEntry>` with retry history and derived `error_class` |
| `trigger_inspect_lifecycle(kind?)` | kind: string or nil | list | Return trigger lifecycle event-log records, optionally filtered by event kind |
| `trigger_inspect_action_graph(trace_id?)` | trace_id: string or nil | list | Return streamed `observability.action_graph` records, optionally filtered to one trace id |
| `trigger_test_harness(fixture)` | fixture: string or `{fixture: string}` | dict | Run a named trigger-system harness fixture and return a structured report. Intended for Rust/unit/conformance coverage of cron, webhook, retry, DLQ, dedupe, rate-limit, cost-guard, recovery, and dead-man-switch scenarios |
| `handler_context()` | — | dict or nil | Return the active trigger dispatch context (`agent`, `action`, `trace_id`, `replay_of_event_id`, `autonomy_tier`, `trigger_event`) or `nil` outside dispatch |
| `trust_record(agent, action, approver, outcome, tier)` | agent: string, action: string, approver: string or nil, outcome: string, tier: string | dict | Append a manual hash-chained `TrustRecord` to `trust_graph` and per-agent topics |
| `trust_graph_record(decision)` | decision: dict | string | Append a hash-chained trust decision and return its `TrustEntryId` |
| `trust_graph_query(agent, action)` | agent: string, action: string or nil | dict | Return a `TrustScore` summary and recommended capability policy for an agent/action pair |
| `trust_graph_policy_for(agent)` | agent: string | dict | Return the capability policy derived from the agent's effective tier and trust history |
| `trust_graph_verify_chain()` | none | dict | Verify the active trust graph hash chain and return `{verified, root_hash, errors, ...}` |
| `trust_query(filters)` | filters: dict | list | Query trust-graph records by `agent`, `action`, `since`, `until`, `tier`, `outcome`, `limit`, and/or `grouped_by_trace` |
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
| `llm_mock(response)` | response: dict | nil | Queue a mock LLM response. Dict supports `text`, `tool_calls`, `match` (glob), `consume_match` (consume a matched pattern instead of reusing it), `input_tokens`, `output_tokens`, `thinking`, `stop_reason`, `model`, `error: {category, message}` (short-circuits the call and surfaces as `VmError::CategorizedError` — useful for testing `llm_call_safe` envelopes and `with_rate_limit` retry loops) |
| `llm_mock_calls()` | — | list | Return list of `{messages, system, tools}` for all calls made to the mock provider |
| `llm_mock_clear()` | — | nil | Clear all queued mock responses and recorded calls |

FIFO mocks (no `match` field) are consumed in order. Pattern-matched mocks
(with `match`) are checked in declaration order against the request transcript
text using glob patterns. They persist by default; add `consume_match: true`
to advance through matching fixtures step by step. When no mocks match, the
default deterministic mock behavior is used.

See [Trigger stdlib](stdlib/triggers.md) for the typed `std/triggers` aliases,
DLQ entry shapes, and the current shallow-path replay / manual-fire caveats.

## Human in the loop

See [Human in the loop](hitl.md) for the full primitive catalog,
event-log topics, bridge contract, and replay semantics.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `ask_user(prompt, options?)` | prompt: string, options: `{schema?: Schema<T>, timeout?: duration, default?: T}` | `T` | Pause the current dispatch until the host supplies a response. Validates against `schema` when present, otherwise coerces toward `default` when possible. Defaults to a 24-hour timeout; on timeout, returns `default` or throws `HumanTimeoutError` |
| `request_approval(action, options?)` | action: string, options: `{detail?: any, quorum?: int, reviewers?: list<string>, deadline?: duration}` | `{approved, reviewers, approved_at, reason, signatures}` | Emit a durable approval request, wait for quorum, and return the approval record with signed reviewer timestamp receipts. Defaults to quorum 1 and a 24-hour deadline. Denial throws `ApprovalDeniedError` |
| `dual_control(n, m, action, approvers?)` | `n: int, m: int, action: fn() -> T, approvers: list<string> or nil` | `T` | n-of-m approval gate for executing `action`. Commonly used for destructive or privileged operations. Denial throws `ApprovalDeniedError` |
| `escalate_to(role, reason)` | role: string, reason: string | `{request_id, role, reason, trace_id, status, accepted_at, reviewer}` | Raise the current dispatch to a higher-trust role and wait for host acceptance. The host or operator resolves it with `harn.hitl.respond` / `harn orchestrator resume` |
| `hitl_pending(filters?)` | filters: `{since?: string, until?: string, kinds?: list<string>, agent?: string, limit?: int}` or `nil` | `list<{request_id, request_kind, agent, prompt, trace_id, timestamp, approvers, metadata}>` | Read the active event log's pending HITL requests as typed rows, newest first. Returns `[]` when no event log is attached. |

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
llm_mock({text: "step 1", match: "*planner*", consume_match: true})
llm_mock({text: "step 2", match: "*planner*", consume_match: true})

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
| `transcript_compact(transcript, options?)` | transcript: dict, options: dict | dict | Compact a transcript with the runtime compaction engine, preserving durable artifacts and compaction events |
| `transcript_auto_compact(messages, options?)` | messages: list, options: dict | list | Apply the agent-loop compaction pipeline to a message list using `llm`, `truncate`, or `custom` strategy |

### Provider configuration

LLM provider endpoints, model aliases, inference rules, and default parameters
are configured via a TOML file. The VM searches for config in this order:

1. Built-in defaults (Anthropic, OpenAI, OpenRouter, HuggingFace, Ollama, Local)
2. `HARN_PROVIDERS_CONFIG` if set, otherwise `~/.config/harn/providers.toml`
3. Installed package `[llm]` tables in `.harn/packages/*/harn.toml`
4. The nearest project `harn.toml` `[llm]` table

The `[llm]` section uses the same schema as `providers.toml`, so project and
package manifests can ship provider adapters declaratively:

```toml
[llm.providers.anthropic]
base_url = "https://api.anthropic.com/v1"
auth_style = "header"
auth_header = "x-api-key"
auth_env = "ANTHROPIC_API_KEY"
chat_endpoint = "/messages"

[llm.providers.local]
base_url = "http://localhost:8000"
base_url_env = "LOCAL_LLM_BASE_URL"
auth_style = "none"
chat_endpoint = "/v1/chat/completions"
completion_endpoint = "/v1/completions"

[llm.aliases]
sonnet = { id = "claude-sonnet-4-20250514", provider = "anthropic" }

[[llm.inference_rules]]
pattern = "claude-*"
provider = "anthropic"

[[llm.tier_rules]]
pattern = "claude-*"
tier = "frontier"

[llm.model_defaults."qwen/*"]
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

## Runtime Context

Logical task, workflow, trigger, agent-session, and trace introspection.
Use this instead of raw OS thread identity.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `runtime_context()` | none | dict | Return the current logical runtime context with task, workflow, trigger, agent, trace, cancellation, debug, and task-local value fields |
| `task_current()` | none | dict | Alias for `runtime_context()` |
| `runtime_context_values()` | none | dict | Return task-local context values for the current logical task |
| `runtime_context_get(key, default?)` | key: string, default: any | any | Return a task-local value, the provided default, or `nil` |
| `runtime_context_set(key, value)` | key: string, value: any | any | Set a task-local value and return the previous value or `nil` |
| `runtime_context_clear(key)` | key: string | any | Clear a task-local value and return the previous value or `nil` |

Children created by `spawn`, `parallel`, `parallel each`, and
`parallel settle` inherit a snapshot of task-local values. Child writes do not
mutate the parent context.

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
| `error_category(err)` | err: any | string | Extract category from a caught error. Returns `"timeout"`, `"auth"`, `"rate_limit"`, `"tool_error"`, `"tool_rejected"`, `"egress_blocked"`, `"cancelled"`, `"not_found"`, `"circuit_open"`, or `"generic"` |
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

## Project introspection

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `project_fingerprint(path?)` | path: string | `ProjectFingerprint` | Return a normalized shallow project profile for the current working directory or the supplied path |

`ProjectFingerprint` has these fields:

- `primary_language`: `"rust"`, `"typescript"`, `"python"`, `"go"`, `"swift"`, `"ruby"`, `"mixed"`, or `"unknown"`
- `languages`: all detected top-level languages in stable order
- `frameworks`: shallow framework signals such as `"axum"`, `"next"`, `"react"`, `"django"`, `"fastapi"`, or `"rails"`
- `package_manager`: the dominant normalized package manager tag such as
  `"cargo"`, `"spm"`, `"pnpm"`, `"npm"`, `"uv"`, `"poetry"`, `"pip"`,
  `"go-mod"`, or `"bundler"`
- `package_managers`: detected package managers such as `"cargo"`, `"npm"`,
  `"pnpm"`, `"yarn"`, `"uv"`, `"poetry"`, `"pip"`, `"go-mod"`, or `"bundler"`
- `test_runner`: the dominant normalized test runner tag such as `"nextest"`,
  `"cargo-test"`, `"vitest"`, `"pytest"`, `"go-test"`, or `"xctest"`
- `build_tool`: the dominant normalized build tool tag such as `"cargo"`,
  `"spm"`, `"next"`, `"vite"`, `"uv"`, `"poetry"`, or `"go"`
- `vcs`: `"git"`, `"hg"`, or `nil` when no VCS root is detected
- `ci`: detected CI providers such as `"github-actions"`, `"gitlab-ci"`,
  `"circleci"`, `"buildkite"`, `"azure-pipelines"`, or `"bitrise"`
- `has_tests`: `true` when a standard test directory such as `tests/`, `test/`, `__tests__/`, or `spec/` is present
- `has_ci`: `true` when CI config such as `.github/workflows/` or `.gitlab-ci.yml` is present
- `lockfile_paths`: relative paths to detected lockfiles such as `Cargo.lock`,
  `package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, or `Package.resolved`

## Secret scanning

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `secret_scan(content)` | content: string | list | Scan text or diffs for high-signal leaked credentials and return redacted findings with detector metadata and source locations |
| `self_review(diff, rubric?, max_rounds?)` | diff: string, rubric: string, max_rounds: int | dict | Run a structured pre-PR self-review over a diff, merge in `secret_scan` blockers, and append a `pr.self_review` trust-graph record with review metadata |

`self_review(...)` uses the existing tier-based model resolver with
`model_tier: "small"` today, so it benefits from Harn's current
provider/model fallback chain without waiting on the broader routing DSL work.

The builtin accepts either a custom rubric string or one of the built-in preset
names:

- `default` — correctness, test coverage, security, and style
- `code` — correctness, regressions, tests, and API compatibility
- `docs` — accuracy, implementation drift, examples, and migration notes
- `infra` — rollout safety, observability, failure modes, and rollback posture
- `security` — credential exposure, auth, data handling, and hardening gaps

It returns a structured result with:

- `summary`
- `findings`
- `has_blocking_findings`
- `rounds`
- `secret_scan_findings`
- `trust_record`

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
as an MCP server using `harn serve mcp`. The CLI serves them over stdio
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
harn serve mcp agent.harn
```

Configure in Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "my-agent": {
      "command": "harn",
      "args": ["serve", "mcp", "agent.harn"]
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
| `friction_event(payload)` | payload: dict | dict | Normalize a redacted friction event for repeated queries, clarifications, approval stalls, missing context, handoffs, tool gaps, failed assumptions, expensive deterministic steps, or human hypotheses |
| `friction_record(payload, options?)` | payload: dict, options: dict | dict | Record a friction event to the process-local buffer, append JSONL with `log_path`/`HARN_FRICTION_LOG`, or no-op when `enabled: false` |
| `friction_events()` | — | list | Return process-local friction events recorded in the current VM |
| `friction_clear()` | — | nil | Clear process-local friction events |
| `context_pack_manifest(payload)` | payload: dict | dict | Validate and normalize a context-pack manifest |
| `context_pack_manifest_parse(src)` | src: TOML or JSON string | dict | Parse and validate a context-pack manifest |
| `context_pack_suggestions(events?, options?)` | events: list or `{events}`, options: dict | list | Generate candidate context-pack/workflow suggestions from repeated friction evidence |
| `friction_eval_fixture(fixture)` | fixture: `{events, options?, expected_suggestions?}` | dict | Evaluate a repeated-friction fixture and assert expected context-pack suggestions |
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

### Workflow messaging and lifecycle

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `workflow.signal(target, name, payload?)` | target, name: string, payload: any | dict | Enqueue a fire-and-forget signal message for a workflow |
| `workflow.query(target, name)` | target, name: string | any | Read the last published query value, or `nil` when absent |
| `workflow.publish_query(target, name, value?)` | target, name: string, value: any | dict | Publish or replace a named query value for a workflow |
| `workflow.update(target, name, payload?, options?)` | target, name: string, payload: any, options: dict | any | Enqueue an update request and wait for a matching response |
| `workflow.receive(target)` | target | dict or nil | Pop the next queued message (`signal`, `update`, or control message) |
| `workflow.respond_update(target, request_id, value, name?)` | target, request_id: string, value: any, name: string (optional) | dict | Fulfill a pending workflow update request |
| `workflow.pause(target)` | target | dict | Mark a workflow paused and enqueue a control message |
| `workflow.resume(target)` | target | dict | Mark a workflow resumed and enqueue a control message |
| `workflow.status(target)` | target | dict | Return mailbox/generation status for a workflow |
| `workflow.continue_as_new(target)` | target | dict | Advance the workflow generation and clear pending update responses |
| `continue_as_new(target)` | target | dict | Top-level alias for `workflow.continue_as_new(...)` |

`target` may be either a workflow-id string or a dict containing
`workflow_id` / `workflow`. The dict form may also include `base_dir`,
`persisted_path`, or `path`; when a persisted run path is provided, Harn
derives the workflow root from the run's parent workspace automatically.

Workflow message state is persisted under
`.harn/workflows/<workflow_id>/state.json` relative to the resolved base
directory. `workflow.update(...)` polls for a response until
`options.timeout_ms` elapses; the default is `30000`.

### Tool lifecycle hooks

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `register_tool_hook(config)` | config: dict | nil | Register a pre/post hook for tool calls matching `pattern` (glob). `deny` string blocks matching tools; `max_output` int truncates results |
| `clear_tool_hooks()` | none | nil | Remove all registered tool hooks |

### Context and compaction utilities

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `assemble_context(options)` | options: dict | dict | Pack artifacts into a token-budgeted slice of chunks with pluggable ranking, cross-artifact dedup, microcompact chunking, and per-chunk observability. See [Adaptive context assembly](#adaptive-context-assembly) below |
| `estimate_tokens(messages)` | messages: list | int | Estimate token count for a message list (chars / 4 heuristic) |
| `microcompact(text, max_chars?)` | text, max_chars (default 20000) | string | Snip oversized text, keeping head and tail with a marker |
| `select_artifacts_adaptive(artifacts, policy)` | artifacts: list, policy: dict | list | Deduplicate, microcompact oversized artifacts, then select with token budget |
| `transcript_auto_compact(messages, options?)` | messages: list, options: dict | list | Run the same transcript auto-compaction pipeline used by `agent_loop` |

#### Adaptive context assembly

`assemble_context` is the within-selection complement to
`transcript_auto_compact`. Where transcript compaction shrinks an
ongoing conversation, `assemble_context` re-packs the next turn's
artifacts into a fixed token budget:

1. Chunk oversized artifacts at paragraph / line boundaries.
2. Dedup chunks across artifacts (exact text match or trigram Jaccard).
3. Rank by recency, keyword overlap against `query`, or a host ranker
   closure.
4. Pack greedy into `budget_tokens`, reporting why each chunk was
   included or dropped.

Options dict:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `artifacts` | `list[artifact]` | required | Source set. Each entry is normalized via `artifact(...)`. |
| `budget_tokens` | int | `8000` | Hard cap on packed tokens. |
| `dedup` | `"none"` / `"chunked"` / `"semantic"` | `"chunked"` | Exact-text hash (`"chunked"`) or trigram Jaccard overlap (`"semantic"`). |
| `semantic_overlap` | float | `0.85` | Jaccard threshold when `dedup: "semantic"`. |
| `strategy` | `"recency"` / `"relevance"` / `"round_robin"` | `"relevance"` | Packing order. |
| `query` | string | nil | Used by the default relevance ranker (keyword overlap + density). |
| `microcompact_threshold` | int | `2000` | Artifacts above this many tokens are chunked. |
| `ranker_callback` | `closure(query, chunks) → list[float]` | nil | Host-supplied ranker. Returns a score per chunk in the same order as the `chunks` input. Only invoked when `strategy: "relevance"`. |

Returned record:

- `chunks: list[chunk]` — selected chunks in pack order. Each carries
  `id`, `artifact_id`, `artifact_kind`, `title`, `source`, `text`,
  `estimated_tokens`, `chunk_index`, `chunk_count`, and `score`.
  `chunk.id = "{artifact_id}#{sha256(text)[..16]}"` — stable and
  content-addressed, so the same input always produces the same id
  across runs for replay diffing.
- `included: list[summary]` — per-artifact `{artifact_id,
  artifact_kind, chunks_included, chunks_total, tokens_included}`.
- `dropped: list[exclusion]` — per-exclusion `{artifact_id, chunk_id,
  reason, detail}`. Reasons include `"no_text"`, `"empty_text"`,
  `"duplicate"`, `"budget_exceeded"`.
- `reasons: list[rationale]` — per-chunk `{chunk_id, artifact_id,
  strategy, score, included, reason}`. Use this to surface "why was
  this in the prompt?" in observability dashboards.
- `total_tokens`, `budget_tokens`, `strategy`, `dedup` echo the
  packing configuration for downstream tooling.

Integration hook: a workflow node may carry `context_assembler: {...}`
in its declaration. When set, `execute_stage_node` routes the
pre-selected artifacts through `assemble_context` and renders the
packed chunks as the stage's prompt context, replacing the default
`render_artifacts_context` output. Scripts that call `agent_loop`
directly can do the same manually: call `assemble_context` on their
artifact list and bake the packed chunks into the system prompt.

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
snapshot/child-run paths, immutable original `request` metadata, normalized
`provenance`, and `audit` mutation-session metadata when available.
The `request` object preserves canonical `research_questions`,
`action_items`, `workflow_stages`, and `verification_steps` arrays when the
caller supplied them.
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
- `response_format: "json"` to parse structured child JSON into `data` from the
  final successful transcript when possible
- `returns: {schema: ...}` to validate that structured child JSON against a
  schema

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
| `agent_session_current_id()` | none | string or nil | Returns the innermost active session id, or `nil` outside any active session |
| `agent_session_length(id)` | id | int | Message count; errors on unknown id |
| `agent_session_snapshot(id)` | id | dict or nil | Read-only transcript snapshot plus `parent_id`, `child_ids`, `branched_at_event_index` |
| `agent_session_ancestry(id)` | id | dict or nil | Returns `{parent_id, child_ids, root_id}` for the current in-VM lineage |
| `agent_session_reset(id)` | id | nil | Wipes history; preserves id and subscribers |
| `agent_session_fork(src, dst?)` | src, dst | string | Copies transcript, sets `dst.parent_id`, and appends `dst` to `src.child_ids` |
| `agent_session_fork_at(src, keep_first, dst?)` | src, keep_first: int, dst | string | Forks then keeps the first `keep_first` messages on the child; records `branched_at_event_index` |
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
| `transcript_compact(transcript, options?)` | transcript, options | transcript | Compact a transcript with the runtime compaction engine |
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
