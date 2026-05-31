# Builtin functions

Complete reference for all built-in functions available in Harn.

## Output

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `log(msg)` | msg: any | nil | Print with `[harn]` prefix and newline |
| `progress(phase, message, progress?, total?)` | phase: string, message: string, optional numeric progress | nil | Emit standalone progress output. Dict options support `mode: "spinner"` with `step`, or `mode: "bar"` with `current`, `total`, and optional `width` |
| `color(text, name)` | text: any, name: string | string | Wrap text with an ANSI foreground color code |
| `bold(text)` | text: any | string | Wrap text with ANSI bold styling |
| `dim(text)` | text: any | string | Wrap text with ANSI dim styling |
| `set_color_mode(mode)` | mode: `"auto"`, `"always"`, or `"never"` | nil | Configure process-local ANSI color behavior for `color`, `bold`, `dim`, and `std/ansi`; `auto` honors TTY detection, `NO_COLOR`, and `FORCE_COLOR` |

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
  log("${u.name} is ${u.age}")
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

log(is_ok(good))             // true
log(is_err(bad))             // true

log(unwrap(good))            // 42
log(unwrap_or(bad, 0))       // 0
log(unwrap_err(bad))         // something went wrong
```

## JSON

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `json_parse(str)` | str: string | value | Parse JSON string into Harn values. Throws on invalid JSON |
| `json_stringify(value)` | value: any | string | Serialize Harn value to JSON. Closures and handles become `null` |
| `json_stringify_pretty(value)` | value: any | string | Serialize Harn value to pretty-printed JSON with stable two-space indentation |
| `yaml_parse(str)` | str: string | value | Parse YAML string into Harn values. Throws on invalid YAML |
| `yaml_stringify(value)` | value: any | string | Serialize Harn value to YAML |
| `toml_parse(str)` | str: string | value | Parse TOML string into Harn values. Throws on invalid TOML |
| `toml_stringify(value)` | value: any | string | Serialize Harn value to TOML |
| `json_validate(data, schema)` | data: any, schema: dict | bool | Validate data against a schema. Returns `true` if valid, throws with details if not |
| `schema_check(data, schema)` | data: any, schema: dict | Result | Validate data against an extended schema and return `Result.Ok(data)` or `Result.Err({message, errors, value?})` |
| `schema_parse(data, schema)` | data: any, schema: dict | Result | Validate data and return `Result.Ok(data)` with `default` values applied recursively, or `Result.Err({message, errors, value?})` |
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

Schema traversal is bounded before validation runs: canonicalization, JSON
Schema/OpenAPI export, runtime validation, and JSON-stream schema setup reject
schemas deeper than 128 nested schema nodes, more than 256 `$ref` expansions,
or cyclic local `$ref` graphs. These failures surface as regular Harn schema
errors such as `schema depth exceeded (128)` or `cyclic schema reference: # -> #`.

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
log(is_ok(parsed))
log(unwrap(parsed).role)
log(schema_to_json_schema(user_schema).type)
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

### JSON pointer and jq-like queries

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
log(title)
log(bytes_to_hex(uploaded))
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
| `repeat(str, n)` | str: string, n: int | string | Concatenate `str` with itself `n` times. Rejects pathological sizes |
| `indent(text, prefix?)` | text: string, prefix: string (default `"  "`) | string | Prefix every non-blank line with the given indent |
| `dedent(text)` | text: string | string | Strip the longest common leading whitespace from every non-blank line |
| `word_wrap(text, width?)` | text: string, width: int (default 80) | string | Greedy word-wrap that preserves existing newlines and never splits words mid-token |

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
| `clone(value)` | value: any | any | Shallow copy. Dicts and lists become fresh allocations independent of the source; primitives return by value |
| `deep_clone(value)` | value: any | any | Recursive deep copy. Nested dicts/lists are duplicated top-to-bottom |
| `deep_merge(a, b)` | a: dict, b: dict | dict | Recursive merge — when both sides have a dict at the same key the dicts merge; otherwise right wins |
| `unique(list)` | list: list | list | Remove duplicates, preserving first-seen order. Structural equality |
| `dict_from_pairs(pairs)` | pairs: list of `[key, value]` | dict | Build a dict from a list of pairs (later pairs override earlier ones) |
| `index_by(items, fn)` | items: list, fn: closure | dict | Build a lookup table keyed by `fn(item)` |
| `to_xml(value, options?)` | value: any, options: dict (`root`, `item_tag`, `pretty`, `declaration`) | string | Convert a Harn value into XML. Dicts → tag trees; lists → repeated `<item>` children |
| `from_xml(text, options?)` | text: string, options: dict (`preserve_repeated_tag`) | dict | Parse XML back into a dict tree. Repeated children collapse into a list by default; `preserve_repeated_tag: true` keeps the inner tag |
| `jsonrpc_call(url, method, params?, options?)` | url: string, method: string, params: any, options: dict (`headers`, `id`, `notify`, `timeout_ms`) | any | POST a JSON-RPC 2.0 envelope. Returns `result` on success, raises a structured error dict on failure |
| `jsonrpc_batch(url, calls, options?)` | url: string, calls: list of `{method, params?, id?, notify?}` dicts, options: dict (`headers`, `timeout_ms`) | list | POST a JSON-RPC 2.0 batch. Returns a list aligned with input order; per-call errors arrive as `{jsonrpc_error: true, ...}` dicts inside the list |

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

Capability-aware scripts should call the `harness.fs.*` methods. The free
filesystem builtins remain supported as thin aliases for existing scripts.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `read_file(path)` | path: string | string | Read entire file as UTF-8 string, or return raw source for embedded `std/...harn.prompt` assets. Throws on failure. **Deprecated in favor of `read_file_result` for new code; the throwing form remains supported.** |
| `read_file_result(path)` | path: string | `Result<string, string>` | Non-throwing read: returns `Result.Ok(content)` on success or `Result.Err(message)` on failure. Shares `read_file`'s content cache for filesystem files and also supports embedded `std/...harn.prompt` assets |
| `write_file(path, content)` | path: string, content: string | nil | Write string to file. Throws on failure |
| `append_file(path, content)` | path: string, content: string | nil | Append string to file, creating it if it doesn't exist. Throws on failure |
| `copy_file(src, dst)` | src: string, dst: string | nil | Copy a file. Throws on failure |
| `delete_file(path)` | path: string | nil | Delete a file or directory (recursive). Throws on failure |
| `file_exists(path)` | path: string | bool | Check if a file or directory exists |
| `list_dir(path?)` | path: string (default `"."`) | list | List directory contents as sorted list of file names. Throws on failure |
| `walk_dir(path, options?)` | path: string, options: dict | list or handle dict | Recursively list files/directories. Options: `max_depth`, `follow_symlinks`, `long_running`/`background` |
| `glob(pattern, base_or_options?, options?)` / `harness.fs.glob(pattern, base_or_options?, options?)` | pattern: string, base: string or options: dict | list or handle dict | Match files under a base directory. Set `long_running`/`background` in options to return a handle |
| `mkdir(path)` | path: string | nil | Create directory and all parent directories. Throws on failure |
| `stat(path)` | path: string | dict | File metadata: `{size, is_file, is_dir, readonly, modified}`. Throws on failure |
| `temp_dir()` | none | string | System temporary directory path |
| `mkdtemp(prefix?)` / `harness.fs.mkdtemp(prefix?)` | prefix: string (default `"harn-"`) | string | Create a new uniquely-named directory under the host temp dir and return its absolute path. The caller owns the directory's lifecycle — it is **not** cleaned up automatically; pair with `harness.fs.delete(path)` when finished. Path separators in `prefix` are stripped so callers cannot smuggle subdirectory trees |
| `render(path, bindings?)` | path: string, bindings: dict | string | Read a template asset and render it. `path` may be source-relative, `@/<rel>` (anchored at the calling file's project root), `@<alias>/<rel>` (anchored at a `[asset_roots]` entry in `harn.toml`), or an embedded `std/...harn.prompt` stdlib prompt asset — see [Package-root prompt assets](./modules.md#package-root-prompt-assets). The template language supports `{{ name }}` interpolation (with nested paths and filters), `{{ if }} / {{ elif }} / {{ else }} / {{ end }}`, `{{ for item in xs }} ... {{ end }}` (with `{{ loop.index }}` etc.), `{{ include "..." }}` partials, logical `{{ section "task" }} ... {{ endsection }}` prompt sections, `{{# comments #}}`, `{{ raw }} ... {{ endraw }}` verbatim blocks, and `{{- -}}` whitespace trim markers. See the [Prompt templating reference](./prompt-templating.md) for the full grammar and filter list. Source-relative paths called from an imported module resolve relative to that module's directory, not the entry pipeline. Without bindings, just reads the file |
| `render_prompt(path, bindings?)` | path: string, bindings: dict | string | Prompt-oriented alias of `render(...)`. Use this for `.harn.prompt` / `.prompt` assets when you want the asset to be surfaced explicitly in bundle manifests and preflight output. Accepts the same `@/<rel>`, `@<alias>/<rel>`, and embedded `std/...harn.prompt` forms as `render(...)` |
| `render_string(template, bindings?)` | template: string, bindings: dict | string | Render an inline template string with the same template engine as `render(...)`. Useful when a library wants to embed a template directly in source instead of shipping a separate `.prompt` file. `{{ include "..." }}` resolves relative to the current module's asset root, with `@/<rel>` and `@<alias>/<rel>` supported |

## Environment and system

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `env(name)` | name: string | string or nil | Read environment variable |
| `env_or(name, default)` | name: string, default: any | string or default | Read environment variable, or return `default` when unset. One-line replacement for the common `let v = env(K); if v { v } else { default }` pattern |
| `timestamp()` | none | float | Unix timestamp in seconds with sub-second precision |
| `elapsed()` | none | int | Milliseconds since VM startup |
| `exec(cmd, args...)` | cmd: string, args: strings | dict | Execute external command. Returns stdout/stderr plus status metadata and `success` |
| `exec_at(dir, cmd, args...)` | dir: string, cmd: string, args: strings | dict | Execute external command inside a specific directory |
| `shell(cmd)` | cmd: string | dict | Execute command via shell. Returns stdout/stderr plus status metadata and `success` |
| `shell_at(dir, cmd)` | dir: string, cmd: string | dict | Execute shell command inside a specific directory |
| `harness.process.spawn_captured(opts)` | opts: dict | dict | Run an external command synchronously through the `Harness` process capability. `opts` is `{cmd, args?, cwd?, env?, stdin?, timeout_ms?}` and supports feeding a stdin payload plus a per-call timeout. Returns `{exit_code, stdout, stderr, duration_ms, success, timed_out}`. On timeout the child is killed, `exit_code = -1`, `success = false`, and `timed_out = true` |
| `spawn_captured(opts)` | opts: dict | dict | Legacy alias for `harness.process.spawn_captured(opts)` when no `Harness` handle is available |
| `term_width()` | none | int | Current terminal column count. Alias for the canonical `harness.term.width()` surface; reads `COLUMNS` env first, falls back to the platform window size, then to `80` |
| `term_height()` | none | int | Current terminal row count. Alias for the canonical `harness.term.height()` surface; reads `LINES` env first, falls back to the platform window size, then to `24` |
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

For interactive terminal presentation, import `std/tui`. It provides
`page({title?, body, format?, no_pager?, footer?})`, `terminal_width()`,
`rule()`, and `clear()` on top of the TTY-aware stdout primitives.

## Regular expressions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `regex_match(pattern, text, flags?)` | pattern: string, text: string, flags: string | list or nil | Find all non-overlapping matches. Optional flags: `i`, `m`, `s`, `x` |
| `regex_split(text, pattern, flags?)` | text: string, pattern: string, flags: string | list | Split text by regex matches |
| `regex_replace(pattern, replacement, text, flags?)` | pattern: string, replacement: string, text: string, flags: string | string | Replace all matches. Optional flags: `i`, `m`, `s`, `x`. Throws on invalid regex |
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
log(encoded)                  // SGVsbG8sIFdvcmxkIQ==
log(base64_decode(encoded))   // Hello, World!
```

```harn
log(base64url_encode(">>>???///"))     // Pj4-Pz8_Ly8v
log(base32_encode("foobar"))           // MZXW6YTBOI======
log(hex_encode("hello"))               // 68656c6c6f
log(hex_decode("68656c6c6f"))          // hello
```

```harn
log(url_encode("hello world"))         // hello%20world
log(url_decode("hello%20world"))       // hello world
log(url_encode("a=1&b=2"))             // a%3D1%26b%3D2
log(url_decode("hello+world"))         // hello world
```

## Hashing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `sha256(string)` | string: string | string | SHA-256 hash, returned as a lowercase hex-encoded string |
| `harness.crypto.sha256(string_or_bytes)` | string_or_bytes: string or bytes | string | Harness-scoped SHA-256 hash, returned as lowercase hex. Accepts `bytes` directly without stringifying, so content-addressing scripts can hash binary payloads unambiguously |
| `sha256_hex(string_or_bytes)` | string_or_bytes: string or bytes | string | Compatibility alias for `harness.crypto.sha256(...)`. For string inputs the digest matches `sha256(...)` |
| `md5(string)` | string: string | string | MD5 hash, returned as a lowercase hex-encoded string |

Example:

```harn
fn main(harness: Harness) {
  log(sha256("hello"))  // 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
  log(harness.crypto.sha256(bytes_from_string("hello")))
  log(md5("hello"))     // 5d41402abc4b2a76b9719d911017c592
}
```

### HMAC and signature comparison

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `hmac_sha256(key, message)` | key: string, message: string | string | HMAC-SHA256 as a lowercase hex-encoded string. Most webhook providers (GitHub, Stripe) send signatures in this form |
| `hmac_sha256_base64(key, message)` | key: string, message: string | string | HMAC-SHA256 as standard base64 (used by Slack-style signatures) |
| `aws_sigv4_headers(spec)` | spec: dict | dict | Deterministically sign one AWS HTTP request with explicit credentials. Returns signed headers plus canonical request metadata; does not perform network I/O |
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

Example (one AWS REST/JSON request against `http_mock`):

```harn
let body = "{\"TableName\":\"Items\"}"
let url = "https://dynamodb.us-east-1.amazonaws.com/"
http_mock("POST", url, {status: 200, body: "{\"ok\":true}", headers: {}})

let signed = aws_sigv4_headers({
  method: "POST",
  url: url,
  service: "dynamodb",
  region: "us-east-1",
  body: body,
  access_key_id: access_key_id,
  secret_access_key: secret_access_key,
  session_token: session_token,
  headers: {
    "Content-Type": "application/x-amz-json-1.0",
    "X-Amz-Target": "DynamoDB_20120810.DescribeTable",
  },
  timestamp: date_format(date_now().timestamp, "%Y%m%dT%H%M%SZ"),
})

let response = harness.net.request("POST", url, {
  body: body,
  headers: signed.headers,
})
```

`aws_sigv4_headers(...)` is intentionally not an AWS SDK. It exists so focused
connector packages can sign a single AWS call when a product workflow needs
one. Credentials must come from the caller or secret provider. `timestamp` is
required to keep signing deterministic; pass a fixed value in tests or format
the current time at the call site. Temporary credentials are supported via
`session_token`, which emits and signs `X-Amz-Security-Token`. Errors name only
invalid fields or signing rules and do not include access keys, secret access
keys, session tokens, derived signing keys, canonical request bodies, or signed
headers. Normal transcript and `http_mock_calls()` redaction treat
`Authorization` and `X-Amz-Security-Token` as sensitive headers.

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
log(parsed.cookies.sid)       // abc
log(parsed.duplicates.sid[1]) // old
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

For civil-time workflows, import `std/calendar`. It provides ISO week and
quarter helpers, start/end-of-period boundaries, DST-explicit local datetime
construction, supported country/timezone metadata, and business-calendar
helpers such as `add_business_days` and `next_business_day`.

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

Path-backed inputs obey the active handler `workspace_roots` read boundary.
Audit events record image metadata and hashes, not raw image bytes.

Example:

```harn
import "std/vision"

let text = ocr("fixtures/receipt.png", {language: "eng"})
log(text.text)
log(text.tokens[0]?.text)
log(text.source.sha256)
```

## Testing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `assert(condition, msg?)` | condition: any, msg: string (optional) | nil | Assert value is truthy. Throws with message on failure |
| `assert_eq(a, b, msg?)` | a: any, b: any, msg: string (optional) | nil | Assert two values are equal. Throws with message on failure |
| `assert_ne(a, b, msg?)` | a: any, b: any, msg: string (optional) | nil | Assert two values are not equal. Throws with message on failure |

## Test results

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `parse_junit_xml(input)` | input: string or bytes | list of dict | Parse a JUnit XML test report into per-case dicts. Lenient: malformed input returns `[]` instead of throwing |

JUnit XML is the de facto interchange format for compiled-language test
runners — GTest with `--gtest_output=xml`, Maven Surefire and Gradle for
JUnit, xUnit/.NET — and is also emitted by pytest, vitest, and
cargo-nextest's JUnit dialect. Each returned dict has the shape:

| Key | Type | Description |
|---|---|---|
| `name` | string | Fully qualified test name (`classname::name` when a `classname` attribute is present, else `name` alone) |
| `status` | string | One of `"passed"`, `"failed"`, `"skipped"`, `"errored"` |
| `duration_ms` | int | `time` attribute in seconds, converted to milliseconds |
| `message` | string or nil | Concatenation of any `<failure>` / `<error>` `message` attribute and child text |
| `stdout` | string or nil | Captured `<system-out>` content |
| `stderr` | string or nil | Captured `<system-err>` content |

```harn
pipeline summarize() {
  let xml = read_file("build/test-results/test-results.xml")
  let cases = parse_junit_xml(xml)
  let failed = cases.filter({ case -> case.status == "failed" || case.status == "errored" })
  log("failures: ${len(failed)} of ${len(cases)}")
  for case in failed {
    log("  ${case.name}: ${case.message}")
  }
}
```

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
`{status: int, headers: dict, body: string, ok: bool, final_url: string}`.
`http_download` returns `{bytes_written, status, headers, ok}`.
Options: `timeout_ms` (alias `timeout`, both in ms), `total_timeout_ms`,
`connect_timeout_ms`, `read_timeout_ms`, `retry: {max, backoff_ms}`,
legacy aliases `retries` / `backoff`, optional `retry_on` (status list),
optional `retry_methods` (defaults to `GET`, `HEAD`, `PUT`, `DELETE`,
`OPTIONS`), `headers` (dict), `query` (dict; nil values omitted and other
values stringified), `auth` (string or `{bearer: "token"}` or
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
and `HARN_EGRESS_DEFAULT=deny`. Under default `harn run`, the worktree
sandbox denies network side effects before destination policy is consulted;
use `egress_policy(...)` for network-enabled host policies or explicit
`--no-sandbox` runs.

```harn
pipeline main(task) {
  egress_policy({
    allow: ["api.example.com:443", "*.trusted.example", "10.0.0.0/8"],
    deny: ["blocked.trusted.example"],
    default: "deny",
  })

  let response = http_get("https://api.example.com/v1/status")
  log(response.status)
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

### std/web

Import deterministic web-source helpers with `import { ... } from "std/web"`.
They reuse the normal Harn HTTP stack, so egress policy, proxies, TLS options,
sessions, retries, and `http_mock` apply exactly as they do for
`http_request`. Sensitive request headers and query params remain governed by
the HTTP mock redaction rules; `std/web` adds source provenance but does not
log response bodies by itself.

| Function | Args | Returns | Description |
|---|---|---|---|
| `web_fetch(url, options?)` | url: string, options: dict | dict | Fetch a source and return `{ok, status, body, headers, content_type, etag, last_modified, fetched_at, cache_status, source_url, final_url, not_modified}` |
| `web_search(query, options?)` | query: string, options: dict | dict | Search curated docs/registry entries, a configured JSON API, provider-hosted result captures, or `HARN_WEB_SEARCH_URL`; returns normalized results with per-result provenance |
| `verify_imports(paths, options?)` | paths: string or list, options: dict | dict | Verify imports in source files against manifests, installed-package hints, registry evidence, symbol metadata, and package trust signals |
| `web_grounding_tools(registry?, options?)` | registry: tool registry or nil, options: dict | tool registry | Add read-only `web_search` and `verify_imports` tools with capability-gated model guidance |
| `web_parse_html(html, source_url?)` | html: string, source_url: string or nil | dict | Extract `{title, meta, canonical_url, links, tables, json_ld, text}` from deterministic HTTP-fetched HTML |
| `web_resolve_url(base_url, href)` | base_url: string, href: string | string or nil | Resolve a relative URL reference against a source URL |
| `web_origin_url(url, path?)` | url: string, path: string | string | Return the URL origin with a replacement path and no query or fragment. Relative paths are rooted at `/` |
| `robots_allowed(url, user_agent?, options?)` | url: string, user_agent: string, options: dict | bool | Check robots.txt using mocked or real HTTP. Missing/non-2xx robots files allow by default |
| `sitemap_urls(base_url, options?)` | base_url: string, options: dict | list of strings | Discover URLs from robots-advertised sitemaps or `/sitemap.xml` |
| `html_title/html_meta/html_links/html_tables/html_json_ld/html_text` | html: string, source_url?: string | value | Convenience projections over `web_parse_html` |

`web_fetch` accepts normal `http_request` options directly, or an explicit
`http_options` dict. Passing `store` from `std/cache` enables recurring
conditional fetches: cached `ETag` and `Last-Modified` values become
`If-None-Match` and `If-Modified-Since` request headers, and a 304 response
returns the cached body with `cache_status: "not_modified"`. Pass `previous`
to use a prior envelope without a cache store, `cache_key` to override the
cache key, `conditional: false` to suppress conditional headers, and
`fetched_at` to pin deterministic fixture timestamps. The default cache key is
method plus the effective request URL after `query` options are applied, so
distinct query variants do not share a cached envelope.

`web_search` is provider-agnostic. Pass `index` or `results` for deterministic
curated entries, `api` for a JSON search service fetched through `web_fetch`,
`provider_results` to normalize externally captured hosted-search results, or
configure `HARN_WEB_SEARCH_URL` for process-level search. Each result includes
`title`, `url`, `snippet`, `source_url`, ranking metadata, and a `provenance`
record with backend id, evidence type, score, source URL, and trust metadata
when available. Result envelopes expose only public backend metadata; configured
API request headers and bodies are not echoed back.

`verify_imports` scans Python, JavaScript/TypeScript, Rust, and Harn imports.
It resolves packages from nearby `package.json`, `pyproject.toml`,
`requirements*.txt`, and `Cargo.toml` manifests, optional
`installed_packages`, and optional registry entries with `symbols`,
`source_url`, `trust_score`, `first_seen`, or `published_at`. The return status
is `pass`, `warn`, or `fail`; `package_not_found` and `symbol_not_found` are
blocking unresolved findings, while `low_trust_package`, `fresh_package`, and
`symbol_unverified` are warnings intended to force a lookup before coding
continues.

`robots_allowed` implements a deterministic robots subset for source-ingest
workflows: exact user-agent groups take precedence over wildcard groups,
multiple matching groups at the same specificity are combined, and
Allow/Disallow decisions use longest-prefix matching with Allow winning ties.

```harn
import { mem_cache } from "std/cache"
import { verify_imports, web_fetch, web_parse_html, web_search, robots_allowed, sitemap_urls } from "std/web"

let store = mem_cache({namespace: "weekly-doc-monitor"})
let page = web_fetch("https://docs.example.com/models", {store: store})
if page.cache_status != "not_modified" && robots_allowed(page.final_url) {
  let parsed = web_parse_html(page.body, page.final_url)
  log(parsed.title)
  log(sitemap_urls(page.final_url))
}

let docs = web_search("fastapi dependency injection", {
  index: [
    {
      title: "FastAPI Dependencies",
      url: "https://fastapi.tiangolo.com/tutorial/dependencies/",
      source_type: "docs",
      authority: true,
    },
  ],
})
let imports = verify_imports("app.py", {
  registry: [{ecosystem: "python", name: "fastapi", symbols: ["FastAPI"]}],
})
```

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
    let accepted = websocket_accept(server, 30000)
    if accepted == nil || accepted?.type == "timeout" {
      continue
    }
    let conn = accepted ?? {}

    let frame = websocket_receive(conn, 30000) ?? {}
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
  log(probe.status)
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
| `http_mock_calls(options?)` | options: dict | list | Return list of `{method, url, headers, body}` for all intercepted calls. Header names are normalized to lowercase. Sensitive request headers and query parameters are redacted by default; pass `{redact_sensitive: false}` or `{include_sensitive: true}` to inspect raw values in tests. |
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

Mocks match and record the final request URL after `query` options are appended:

```harn
http_mock("GET", "https://api.example.com/users?limit=2", {status: 200})
http_get("https://api.example.com/users", {
  headers: {Authorization: "Bearer test-token"},
  query: {limit: 2},
})
let call = http_mock_calls({redact_sensitive: false})[0]
assert_eq(call.url, "https://api.example.com/users?limit=2")
assert_eq(call.headers.authorization, "Bearer test-token")
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
| `pg_stmt_cache_clear(pool)` | pool: dict | dict | Clear prepared-statement caches on idle primary and replica connections |
| `pg.jsonb.path(pool, document, jsonpath)` | pool: dict, document: any, jsonpath: string | list | Run `jsonb_path_query` with bound operands |
| `pg.jsonb.merge(pool, left, right)` | pool: dict, left: any, right: any | any | Merge two JSONB values with Postgres `\|\|` semantics |
| `pg.jsonb.contains(pool, left, right)` | pool: dict, left: any, right: any | bool | Test JSONB containment with Postgres `@>` semantics |
| `pg_mock_pool(fixtures)` | fixtures: list | dict | Create a fixture-backed Postgres handle for tests |
| `pg_mock_calls(mock)` | mock: dict | list | Return recorded mock SQL calls |

Connection sources may be raw Postgres URLs, `env:NAME`, `secret:namespace/name`,
or `{url}`, `{env}`, or `{secret}` dictionaries. Pool options include
`max_connections`, `min_connections`, `acquire_timeout_ms`, `idle_timeout_ms`,
`max_lifetime_ms`, `ssl_mode`, `application_name`, and
`statement_cache_capacity`. Pool options also accept `replicas` and
`read_routing_policy`.

`pg_stmt_cache_clear(pool)` returns `{pools, connections_cleared,
connections_skipped}`. It does not close or recreate the pool; checked-out
connections are skipped so in-flight queries are not interrupted.

Use `params` for every dynamic value:

```harn
let rows = pg_query(
  db,
  "select id, payload from receipts where tenant_id = $1 and id = $2::uuid",
  [tenant_id, receipt_id],
)
```

Rows decode into dictionaries. JSON/JSONB becomes Harn values; UUID, date,
time, timestamp, and timestamptz decode as strings; hstore, ranges, and
geometric types decode as structured dictionaries. Transaction `options` may
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
| `read_stdin()` | — | string or nil | Read the remaining stdin contents |
| `is_stdin_tty()` / `is_stdout_tty()` / `is_stderr_tty()` | — | bool | Return whether the corresponding process stream is attached to a terminal |

Ambient stdio helpers were removed. Use `harness.stdio.print`,
`harness.stdio.println`, `harness.stdio.eprint`,
`harness.stdio.eprintln`, `harness.stdio.read_line`, and
`harness.stdio.prompt` from code that already has a `Harness` handle. Use
`harness.term.width()`, `harness.term.height()`, and
`harness.term.read_password(prompt?)` for terminal dimensions and no-echo
password input. For structured interactive input outside a harness entrypoint,
import `read_line`, `read_password`, `is_tty`, and `write_stderr` from
`std/io`.

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

The `process` capability includes the shell discovery contract used by
shell-mode command runners:

- `process.list_shells` returns `{shells, default_shell_id}` with stable shell
  IDs, paths, platform, availability, invocation args, and source.
- `process.get_default_shell` returns the selected shell object for the
  current host/session.
- `process.set_default_shell` selects a shell by `shell_id` for stateful hosts.
- `process.shell_invocation` resolves `{shell_id?, shell?, command?, login?,
  interactive?}` to `{program, args, command_arg_index, shell}`. When neither
  `shell_id` nor `shell` is supplied, it uses the selected default shell.

Programmatic execution should prefer argv mode (`process.exec` with
`mode: "argv"`). Shell mode is for user-authored commands; callers can pass a
shell object or shell ID discovered through this capability, or omit both to use
the selected default shell.

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

`std/testing` also includes persona steel-thread assertions:
`step_assertions_begin`, `step_events`, `assert_steps_ran`,
`assert_step_received`, `assert_step_emitted`, `assert_handoff_emitted`,
`assert_receipt_field`, and `assert_golden_transcript`. These helpers record
existing `PreStep` / `PostStep` hook payloads and assert Harn-level step,
handoff, receipt, and structured transcript boundaries.

## Git stdlib

The `git` namespace provides typed local repository operations over the
runtime command runner. These are local subprocess operations, not forge
connector calls. Every operation returns a `harn-stdlib-git-receipt-v1`
envelope with command args, working directory, status, exit category, output
refs, affected paths, agent identity, trace id, command-policy audit data, and
operation data. Receipts are appended to `stdlib.git.receipts`; each operation
also writes a TrustGraph record.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `git.status(repo)` | repo: path string or repo dict | `GitReceipt` | Run `git status --porcelain=v1 --branch` and return structured entries |
| `git.conflicts(repo)` | repo: path string or repo dict | `GitReceipt` | Return structured unmerged paths and conflict kinds where git exposes them |
| `git.fetch(repo, remote, refspecs)` | repo: path/dict, remote: string, refspecs: list of strings | `GitReceipt` | Fetch from an existing local remote configuration |
| `git.rebase(repo, base_ref)` | repo: path/dict, base_ref: string | `GitReceipt` | Rebase the current branch onto `base_ref`; treated as risky for approval/autonomy |
| `git.push(repo, remote, refspec, lease?)` | repo: path/dict, remote: string, refspec: string, lease: `{ref?, expected_oid}` or oid string | `GitReceipt` | Push a refspec. Force-with-lease requires an expected remote OID and fails with `lease_mismatch` if the remote advanced |
| `git.diff(repo, selector?)` | selector: range string, path list, or `{range?, paths?}` | `GitReceipt` | Return diff text for a range and/or paths |
| `git.merge_base(repo, left, right)` | left/right: refs | `GitReceipt` | Return the merge-base OID |
| `git.repo_discover(path)` | path: string | `GitReceipt` | Discover repository root/git-dir metadata |
| `git.worktree_create(repo, branch, path, options?)` | options: `{base_ref?, force?, detach?}` | `GitReceipt` | Create a worktree using argv-mode git |
| `git.worktree_remove(path, options?)` | options: `{force?}` | `GitReceipt` | Remove a worktree; missing paths return an idempotent `status: "no_op"` receipt |

Canonical builtin names are also registered as `git.repo.discover`,
`git.worktree.create`, and `git.worktree.remove`. The root aliases above are
the ergonomic Harn call surface for nested operations.

Import `std/git` for first-class Harn function wrappers around this namespace
plus local argv-mode helpers that are suitable as granular agent tools. The
receipt-producing wrappers (`git_status`, `git_diff`, `git_rebase`,
`git_push`, and worktree helpers) delegate to the audited builtins above. The
local command helpers (`git_run`, `git_current_branch`, `git_log`,
`git_switch`, `git_pull_ff_only`, `git_fetch`, `git_branch_list`, and
`git_remote_list`) run `git` through argv-mode `process.exec` with ambient git
environment overrides removed.

`git_tools(registry?, options?)` builds a selected tool registry from those
functions. It defaults to read-only git inspection helpers and only exposes
checkout-changing helpers such as `git_switch` or `git_pull_ff_only` when they
are explicitly included in `enabled_tools`. It accepts the same Tool Vault
metadata knobs used by host tool helpers: `defer_loading`, `namespace`, and
per-tool `tool_config`.

For small/local models, `git_toolbox_tools(registry?, options?)` exposes a
compact two-tool surface: `find_git_tool` ranks available git operations with a
deterministic lexical scorer, and `run_git_tool` executes one returned operation
id. Mutating operations stay unavailable unless the toolbox is configured with
`include_mutations: true`.

## Command policy

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `command_policy(config)` | config: dict | dict | Normalize a command-runner policy with workspace roots, deterministic deny/approval rules, and optional pre/post closures |
| `command_policy_push(policy)` | policy: dict | nil | Install a command policy for the current VM scope |
| `command_policy_pop()` | — | nil | Remove the most recently installed command policy |
| `with_autonomy_policy(policy, fn)` | policy: dict, fn: closure | whatever `fn` returns | Run `fn` with a scoped autonomy tier policy; side-effecting builtins are enforced by the VM |
| `with_execution_policy(policy, fn)` | policy: dict, fn: closure | whatever `fn` returns | Run `fn` with a scoped capability policy; the policy is popped on success or throw |
| `with_approval_policy(policy, fn)` | policy: dict, fn: closure | whatever `fn` returns | Run `fn` with a scoped tool approval policy; the policy is popped on success or throw |
| `with_command_policy(policy, fn)` | policy: dict, fn: closure | whatever `fn` returns | Run `fn` with a scoped command policy; the policy is popped on success or throw |
| `with_dynamic_permissions(policy, fn)` | policy: dict, fn: closure | whatever `fn` returns | Run `fn` with a scoped dynamic permission policy; the policy is popped on success or throw |
| `command_risk_scan(ctx)` | ctx: dict | dict | Run deterministic command-risk classification and return labels, confidence, rationale, and recommended action |
| `command_result_scan(ctx)` | ctx: dict | dict | Classify a command result envelope for unsafe output or audit annotations |
| `command_llm_risk_scan(ctx, options?)` | ctx: dict, options: dict | dict | Return the structured risk-scan helper shape with redacted options; deterministic fallback does not make network calls |

Install policies directly or pass them to `agent_loop` with
`command_policy: policy` / `policy: {command_policy: policy}`. Active policies
wrap `host_call("process.exec", ...)`: pre-hooks run before spawn, blocked
decisions return a `status: "blocked"` envelope without starting a child
process, post-hooks can annotate the command audit, and hook recursion is
denied unless `allow_recursive: true`.

## Async and timing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `sleep(duration)` | duration: int (ms) or duration literal | nil | Pause execution |

## Durable agent channels

Durable channels publish structured agent/runtime facts into the active event
log instead of an in-process `channel(...)` handle. Use them when a fact should
be visible to trigger orchestration, later workers, or audit readers.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `emit_channel(name, payload, options?)` | name: string, payload: any, options: `{id?: string, scope?: "session" \| "pipeline" \| "tenant" \| "org", tenant_id?: string, session_id?: string, pipeline_id?: string, ttl?: duration}` | dict | Append a durable channel event and return `{event_id, id, name_resolved, scope, scope_id, emitted_at, emitted_by, retention, duplicate}`. Bare names default to tenant scope. Reusing the same `id` on the same resolved channel is a no-op and returns the original `event_id`. |
| `channel_events(name, options?)` | name: string, options: `{scope?: string, from_cursor?: int, cursor?: int, limit?: int}` | list | Read stored channel events for the resolved channel, oldest first. Intended for tests, diagnostics, and local orchestration inspection. |

Channel names resolve to `scope:scope_id:name`. Bare
`emit_channel("pr.merged", payload)` resolves to
`tenant:<current-or-default-tenant>:pr.merged`. Prefixes select a scope:
`session:foo`, `pipeline:foo`, `tenant:foo`, `tenant:<tenant_id>:foo`, and
`org:<org_id>:foo`. Session events are retained in the current process,
pipeline and tenant events use the active durable event log, and org-scoped
channels currently fail with `HARN-CHN-002` until org grants are available.
Every stored event includes a signed `emitted_at` timestamp, `emitted_by`, the
fully resolved name, any available `pipeline_id`, `session_id`, or `tenant_id`,
and `ttl_ms` when `options.ttl` is provided.

The resolver enforces a four-tier hierarchy (`session` < `pipeline` < `tenant`
< `org`) deterministically. Cross-scope isolation is automatic: distinct
`tenant_id`, `session_id`, or `pipeline_id` values resolve to distinct topics,
so a reader against a different scope id sees an empty view. Diagnostic codes
emitted by the resolver:

| Code | When |
|---|---|
| `HARN-CHN-001` | `pipeline:` scope used outside any pipeline context. |
| `HARN-CHN-002` | Cross-tenant emit without a grant, or `org:` scope (disabled in v1). |
| `HARN-CHN-003` | Malformed channel name, scope prefix, or scope id. |
| `HARN-CHN-004` | Scope ambiguous — explicit `options.session_id` or `options.pipeline_id` conflicts with the active runtime context. |

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
| `llm_call(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Single LLM request. Returns `{text, model, provider, input_tokens, output_tokens, usage, prose, visible_text, blocks, transcript?, tool_calls?, stop_reason?, data?}`. Supports `budget: {max_cost_usd?, max_input_tokens?, max_output_tokens?, total_budget_usd?}` pre-flight checks and throws on transport / rate-limit / budget / schema-validation failures |
| `llm_call_safe(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Non-throwing envelope around `llm_call`. Returns `{ok: bool, response: llm_call result or nil, error: {category, kind, reason, message, provider?, model?, status?, retry_after_ms?} or nil}`. `error.category` is one of `ErrorCategory`'s canonical strings (`"rate_limit"`, `"timeout"`, `"overloaded"`, `"server_error"`, `"transient_network"`, `"schema_validation"`, `"auth"`, `"not_found"`, `"circuit_open"`, `"budget_exceeded"`, `"tool_error"`, `"tool_rejected"`, `"egress_blocked"`, `"cancelled"`, `"generic"`) |
| `llm_stream_call(prompt, system?, options?)` | prompt: string, system: string, options: dict | stream | Streaming LLM request. Returns `Stream<{delta, visible_delta, partial, role, finish_reason}>`; dropping the stream cancels the background request. Uses the same options as `llm_call`; the `stream` option remains the transport toggle |
| `with_rate_limit(provider, fn, options?)` | provider: string, fn: closure, options: dict | whatever `fn` returns | Acquire a permit from the provider's sliding-window rate limiter, invoke `fn`, and retry with exponential backoff on retryable errors (`rate_limit`, `overloaded`, `transient_network`, `timeout`). Options: `max_retries` (default 5), `backoff_ms` (default 1000, capped at 30s after doubling) |
| `llm_completion(prefix, suffix?, system?, options?)` | prefix: string, suffix: string, system: string, options: dict | dict | Text completion / fill-in-the-middle request. Returns the same result shape as `llm_call` |
| `agent_loop(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Multi-turn agent loop with natural completion for native-tool loops, cooperative worker suspension (`status: "suspended"` with a resumable `handle`), sentinel completion for text/no-tool loops (`<done>##DONE##</done>` in tagged text-tool stages), daemon/idling support, optional per-turn context filtering, session-local scratchpad recitation/reorganization, opt-in repeated-tool-call stall diagnostics, and structured provider/tool-protocol failure capture. Returns `{status, error?, text, visible_text, llm: {iterations, duration_ms, input_tokens, output_tokens}, tools: {calls, successful, rejected, mode}, transcript, task_ledger, trace, …}` |
| `agent_progress(input)` | input: dict | nil | Emit a `progress_reported` agent event for the current session. `input` requires either `message: string` or `entries: [{content, status, priority?}]`; `replace` defaults to `true`, and `metadata` defaults to `{}` |
| `agent_turn(prompt, options?)` | prompt: string, options: dict | dict | High-level agent turn wrapper around `agent_loop`. It installs generic user-visible progress guidance, requires the completion judge (`done_judge`), defaults to loop-until-done completion (natural for native-tool turns, sentinel-based for text/no-tool turns), and returns the normal loop result plus `iterations` and `judge_decisions` summaries |
| `agent_llm_turn(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Low-level one-turn LLM request used by stdlib orchestration; equivalent to `llm_call` but intentionally lives under the agent primitive surface |
| `agent_parse_tool_calls(text, tools?)` | text: string, tools: registry or nil | dict | Parse tagged/text-mode tool calls into `{tool_calls, prose, canonical_text, protocol_violations, tool_parse_errors, done_marker}` |
| `agent_dispatch_tool_call(call, tools?, options?)` | call: dict, tools: registry or nil, options: dict | dict | Dispatch one normalized tool call through the runtime parser/enforcement path and return `{ok, status, rendered_result, error_category, executor, ...}` |
| `agent_dispatch_tool_batch(calls, tools?, options?)` | calls: list, tools: registry or nil, options: dict | list | Dispatch a list of normalized tool calls through the host batch primitive and return one result envelope per call; a leading read-only run may execute in parallel |
| `daemon_spawn(config)` | config: dict | dict | Start a daemon-mode agent and return a daemon handle with persisted state + queue metadata |
| `daemon_trigger(handle, event)` | handle: dict or string, event: any | dict | Enqueue a durable FIFO trigger event for a running daemon; throws `VmError::DaemonQueueFull` on overflow |
| `daemon_snapshot(handle)` | handle: dict or string | dict | Return the latest daemon snapshot plus live queue state (`pending_events`, `inflight_event`, counts, capacity) |
| `daemon_stop(handle)` | handle: dict or string | dict | Stop a daemon and preserve queued trigger state for resume |
| `daemon_resume(path)` | path: string | dict | Resume a daemon from its persisted state directory |
| `external_agent_delegate(target, task, options?)` | target: string, task: string, options: dict | dict | Delegate to an open A2A external agent using `harn.external_agent.v1`; returns a checkpoint envelope before dispatch, enforces hard budget/idempotency capability checks, and normalizes completed work into reviewable handoff/diff artifacts |
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
| `trust.query(filters)` | filters: dict | list | Query compact `TrustGraphRecord` rows by `actor`/`actor_id`/`agent`, `action`, `outcome`, `since`, `until`, `autonomy_tier_at_time`, and `limit` |
| `trust.record(decision)` | decision: dict | string | Append a trust decision and return its `TrustEntryId`; accepts `actor_id`, `action`, `approver`, `outcome`, `trace_id`, `autonomy_tier_at_time`, `evidence_refs`, `cost_usd`, and `metadata` |
| `trust.score(actor_id, action)` | actor_id: string, action: string or nil | dict | Return aggregate trust counters and the derived capability policy |
| `trust.policy_for(actor_id)` | actor_id: string | dict | Return only the derived capability policy |
| `trust.verify_chain()` | none | dict | Verify the underlying OpenTrustGraph hash chain |
| `llm_info()` | — | dict | Current LLM config: `{provider, model, api_key_set}` |
| `runtime_introspection()` | — | dict | Full resolved runtime snapshot: `{provider, model, model_alias, family, tool_format, tier, context_window, runtime_context_window, capabilities, harn_version, harness}`. Fields stay `nil` until the first `llm_call` on the thread; `harn_version` and `harness` are always populated. See [Runtime introspection tools](./stdlib/runtime-introspection.md) for the model-callable tool surface (`runtime_introspection_tools(reg)`). |
| `llm_usage()` | — | dict | Cumulative usage: `{input_tokens, output_tokens, total_duration_ms, call_count, total_calls}` |
| `llm_resolve_model(alias)` | alias: string | dict | Resolve model alias or provider-prefixed selector to `{id, provider, alias, tool_format, tier, family, lineage}` via providers.toml |
| `llm_model_info(model)` | model: string | dict | Return resolved model/provider metadata plus normalized `family`/`lineage`, catalog entry, capabilities, API-key availability, and QC default |
| `llm_pick_model(target, options?)` | target: string, options: dict | dict | Resolve a model alias or tier to `{id, provider, tier}` |
| `llm_complementary_reviewer(options)` | options: `{author_model, author_provider?, intent?, max_price_multiplier?}` | dict | Pick a different-family reviewer model for `review`, `critique`, or `plan_review`, returning the selected model, fallback reason when needed, and estimated incremental cost |
| `llm_infer_provider(model_id)` | model_id: string | string | Infer provider from model ID (e.g. `"claude-*"` → `"anthropic"`) |
| `llm_model_tier(model_id)` | model_id: string | string | Get capability tier: `"small"`, `"mid"`, or `"frontier"` |
| `llm_healthcheck(provider?, options?)` | provider: string or `{provider, api_key?, model?}`, options: `{api_key?, model?}` or model string | dict | Validate a configured provider healthcheck. Returns `{provider, valid, message, metadata}`; `api_key` lets hosts validate a candidate key without first exporting it. For OpenAI-compatible `/models` healthchecks, passing a `model` (positional, `{model: "..."}`, or `{provider, model: "..."}`) verifies the selected model/alias is served and surfaces distinct `metadata.category` values such as `unreachable`, `bad_status`, `model_missing`, and `invalid_url` |
| `llm_apply_reasoning_policy(opts)` | opts: dict | dict | Apply Harn's provider-aware `reasoning_policy` / `thinking_policy` lowering to an `llm_call` option dict, preserving caller-supplied `thinking` or `reasoning_effort` |
| `llm_rate_limit(provider, options?)` | provider: string, options: dict | int/nil/bool | Set (`{rpm: N}`), query, or clear (`{rpm: 0}`) per-provider rate limit |
| `llm_providers()` | — | list | List all configured provider names |
| `harness.llm.providers()` | — | list | Per-provider availability + credential snapshot: `[{name, available, credential_status}, ...]`. `credential_status` is one of `"ok"`, `"missing"`, `"not_required"`, `"deferred"` |
| `llm_provider_status()` | — | list | Free-builtin alias for `harness.llm.providers()`, available to scripts that do not receive a `Harness` parameter |
| `llm_available_providers()` | — | list | List providers usable in the current environment (auth configured or no auth required) |
| `llm_known_models()` | — | list | List configured model alias names |
| `llm_qc_default_model(provider)` | provider: string | string/nil | Return the configured cheap QC/repair model for a provider, honoring `BURIN_QC_MODEL` |
| `llm_provider_catalog()` | — | dict | Return the loaded provider/model catalog: providers, aliases, model metadata, normalized family/lineage, pricing, QC defaults, and availability |
| `harness.llm.catalog()` | — | list | Return the full configured model catalog as a list of dicts: `[{id, name, provider, context_window, runtime_context_window, capabilities, quality_tags, pricing, availability, deprecated, deprecation_note, ...}, ...]`. Read-only view used by `harn models list` / `harn models recommend` |
| `harness.llm.catalog_refresh(options?)` | `options?: dict\|nil` | dict | Refresh the process-wide provider/model catalog overlay from the configured hosted catalog, validating the remote document before installing it |
| `llm_catalog()` | — | list | Free-builtin alias for `harness.llm.catalog()`, available to scripts that do not receive a `Harness` parameter |
| `llm_catalog_refresh(options?)` | `options?: dict\|nil` | dict | Free-builtin alias for `harness.llm.catalog_refresh(options?)`, available to scripts that do not receive a `Harness` parameter |
| `llm_config(provider?)` | provider: string | dict | Get provider config (base_url, auth_style, etc.) |
| `llm_cost(model, input_tokens, output_tokens)` | model: string, input_tokens: int, output_tokens: int | float | Estimate USD cost from catalog pricing, falling back to embedded pricing |
| `llm_session_cost()` | — | dict | Session totals: `{total_cost, input_tokens, output_tokens, call_count}` |
| `llm_budget(max_cost)` | max_cost: float | nil | Set session budget in USD. LLM calls pre-flight and throw if projected cost would exceed it |
| `llm_budget_remaining()` | — | float or nil | Remaining budget (nil if no budget set) |
| `tiktoken_count_tokens(text, model)` | text: string, model: string | int | Count text with the selected tiktoken encoder for known OpenAI models and labeled Claude/Gemini approximations |
| `tiktoken_tokenizer_info(model)` | model: string | dict | Return `{model, model_family, source, exact, known_model_family, encoder}` for the encoder or heuristic fallback used by a model ID |
| `llm_mock(response)` | response: dict | nil | Queue a mock LLM response. Dict supports `text`, `tool_calls`, `blocks`, `logprobs`, `match` (glob), `consume_match` (consume a matched pattern instead of reusing it), `input_tokens`, `output_tokens`, `thinking`, `stop_reason`, `provider`, `model`, `error: {category, message?, retry_after_ms?}` or provider envelopes `error: {status, kind, reason?, message?, retry_after_ms?}` (short-circuits the call and surfaces the same structured error dict as live provider failures — useful for testing `llm_call_safe` envelopes and retry loops) |
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
| `request_approval(action, options?)` | action: string, options: `{detail?: any, args?: any, quorum?: int, reviewers?: list<string>, deadline?: duration, principal?: string, evidence_refs?: list<dict>, undo_metadata?: dict, capabilities_requested?: list<string>}` | `{approved, reviewers, approved_at, reason, signatures}` | Emit a durable approval request, wait for quorum, and return the approval record with signed reviewer timestamp receipts. Defaults to quorum 1 and a 24-hour deadline. Denial throws `ApprovalDeniedError` |
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
llm_mock({error: {status: 503, kind: "transient", reason: "upstream_unavailable"}})

// Inspect what was sent
let calls = llm_mock_calls()
llm_mock_clear()
```

### Transcript helpers

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `transcript(metadata?)` | metadata: dict | transcript | Create a new transcript |
| `transcript_from_messages(messages_or_transcript)` | list or dict | transcript | Normalize a message list into a transcript |
| `transcript_messages(transcript)` | transcript: dict | list | Get transcript messages |
| `transcript_summary(transcript)` | transcript: dict | string or nil | Get transcript summary |
| `transcript_id(transcript)` | transcript: dict | string | Get transcript id |
| `transcript_export(transcript)` | transcript: dict | string | Export transcript as JSON |
| `transcript_import(json_text)` | json_text: string | dict | Import transcript JSON |
| `transcript_fork(transcript, options?)` | transcript: dict, options: dict | transcript | Fork transcript, optionally dropping messages or summary |
| `transcript.inject_reminder(transcript, options)` | transcript: dict, options: dict | dict | Return `{transcript, reminder_id, deduped_count}` after appending a pending `system_reminder` event. `body` is required; `tags`, `dedupe_key`, `ttl_turns`, `preserve_on_compact`, `propagate`, and `role_hint` are optional and validated. A matching `dedupe_key` replaces older pending reminders and emits a lifecycle event when EventLog is active. |
| `transcript.clear_reminders(transcript, selector)` | transcript: dict, selector: dict | dict | Return `{transcript, removed_count}` after removing pending reminders selected by `id`, `tag`, or `dedupe_key`; multiple selectors are combined with AND semantics. |
| `transcript_summarize(transcript, options?)` | transcript: dict, options: dict | transcript | Summarize and compact a transcript via `llm_call` |
| `transcript_compact(transcript, options?)` | transcript: dict, options: dict | transcript | Compact a transcript with the runtime compaction engine, preserving durable artifacts and compaction events. Pending reminders are TTL-processed and deduped before compaction; only `preserve_on_compact: true` reminders survive verbatim. `strategy: "custom"` requires `custom_compactor`, a closure called with `(messages, reminders)` that returns the replacement transcript state |
| `transcript_auto_compact(messages, options?)` | messages: list, options: dict | list | Apply the agent-loop compaction pipeline to a message list using `llm`, `truncate`, or `custom` strategy |

### Provider configuration

LLM provider endpoints, model aliases, inference rules, and default parameters
are configured via a TOML file. The VM searches for config in this order:

1. Built-in defaults (Anthropic, OpenAI, OpenRouter, HuggingFace, Ollama, Local, llama.cpp)
2. `HARN_PROVIDERS_CONFIG` if set, otherwise `~/.config/harn/providers.toml`
3. Installed package `[llm]` tables in `.harn/packages/*/harn.toml`
4. The nearest project `harn.toml` `[llm]` table

The files in steps 2-4 are overlays on the built-in defaults. The `[llm]`
section uses the same schema as `providers.toml`, so project and package
manifests can ship provider adapters declaratively:

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

[llm.providers.llamacpp]
base_url = "http://127.0.0.1:8001"
base_url_env = "LLAMACPP_BASE_URL"
auth_style = "none"
chat_endpoint = "/v1/chat/completions"
completion_endpoint = "/v1/completions"

[llm.aliases]
sonnet = { id = "claude-sonnet-4-6", provider = "anthropic" }

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
    log("circuit open, skipping call")
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

## Runtime context

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

log(trace_summary())
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
log("LLM calls: " + str(summary.llm_calls))
log("Tools used: " + str(summary.tools_used))
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
    log("will retry after backoff")
  }
  log(error_category(e))  // "timeout"
}
```

## Tool registry (low-level)

Low-level tool management functions for building and inspecting tool
registries programmatically. For MCP serving, see the `tool_define` /
`mcp_tools` API above.

For declarative batches, `import { tool_define_many, tool_registry_from } from
"std/tools"`. `tool_define_many(registry, specs: list<ToolDefinitionSpec>)`
adds typed `{name, description, parameters, handler, ...}` specs to a
`ToolRegistry`, and `tool_registry_from(specs)` creates a fresh registry from
the same shape.

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

`composition_binding_manifest(tool_list(registry))` projects a registry into a
prompt-visible Code Mode API. The default manifest hides denied tools; pass
`{include_denied: true}` only when producing audit/debug examples.

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

Directory entries inherit; file entries do not. The shard schema is shared:
each namespace shard has a directory map under `entries` and an optional
file map under `files`, both keyed by normalized relative path. Shards
without a `files` section load unchanged.

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
| `path_metadata_get(path, namespace?, opts?)` | path: string, namespace: string, opts: `{kind?: "file"\|"dir"}` | dict \| nil | Read metadata at an exact path. File entries (default) do not inherit; `{kind: "dir"}` falls back to hierarchical resolution. |
| `path_metadata_set(path, namespace, data, opts?)` | path: string, namespace: string, data: dict, opts: `{kind?: "file"\|"dir"}` | nil | Write metadata at an exact path. Defaults to `{kind: "file"}`. |
| `path_metadata_entries(namespace?, opts?)` | namespace: string, opts: `{kind?: "file"\|"dir"\|"all"}` | list | List stored entries keyed by normalized relative path. Defaults to files only. |
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
| `mcp_roots()` / `harn.mcp.roots()` | none | list | Return the MCP roots Harn exposes to connected servers (`uri`, `name`, `path`) |
| `mcp_configure(config)` / `harn.mcp.configure(config)` | config: dict | dict | Opt into experimental MCP behavior for the current VM, including draft SEP-2356 file inputs |
| `mcp_file_input(options?)` / `harn.mcp.file_input(options?)` | options: dict | dict | Return a JSON Schema property using the draft `x-mcp-file` annotation |
| `mcp_upload_file(server, file_path, options?)` / `harn.mcp.upload_file(server, file_path, options?)` | server: mcp\_client, file\_path: string, options: dict | string | Encode a local file as an RFC 2397 `data:` URI for an experimental MCP file input |
| `mcp_connect(command, args?, options?)` | command: string, args: list, options: dict | mcp\_client | Spawn an MCP server and connect with the legacy or opt-in RC client profile |
| `mcp_list_tools(client)` | client: mcp\_client | list | List available tools from the server |
| `mcp_call(client, name, arguments?)` | client: mcp\_client, name: string, arguments: dict | string or list | Call a tool and return the result |
| `mcp_list_resources(client)` | client: mcp\_client | list | List available resources from the server |
| `mcp_list_resource_templates(client)` | client: mcp\_client | list | List resource templates (URI templates) from the server |
| `mcp_read_resource(client, uri)` | client: mcp\_client, uri: string | string or list | Read a resource by URI |
| `mcp_list_prompts(client)` | client: mcp\_client | list | List available prompts from the server |
| `mcp_get_prompt(client, name, arguments?)` | client: mcp\_client, name: string, arguments: dict | dict | Get a prompt with optional arguments |
| `mcp_server_info(client)` | client: mcp\_client | dict | Get connection info (`name`, `connected`) plus the server initialize response and extracted advisory `instructions` when supplied |
| `mcp_disconnect(client)` | client: mcp\_client | nil | Kill the server process and release resources |

Example:

```harn
let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
let tools = mcp_list_tools(client)
log(tools)

let result = mcp_call(client, "read_file", {"path": "/tmp/hello.txt"})
log(result)

mcp_disconnect(client)
```

Notes:

- MCP file inputs are experimental and default off. Harn implements the current
  draft [SEP-2356](https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2356)
  shape: `x-mcp-file` on `uri` string schema properties and inline RFC 2397
  `data:` URI values. Enable it explicitly with
  `harn.mcp.configure({experimental: {file_upload: {spec_revision:
  "modelcontextprotocol/modelcontextprotocol#2356"}}})`.
- `mcp_call` returns a string when the tool produces a single text block,
  a list of content dicts for multi-block results, or nil when empty.
- HTTP MCP clients keep the server's Streamable HTTP GET event stream open
  after legacy initialization. RC clients use stateless request/response HTTP
  with per-request metadata instead.
- Set `options.protocol_mode` to `"rc"` for direct stdio connects, or
  `protocol_mode = "rc"` in `harn.toml`, to opt into the draft MCP client
  profile. Harn probes RC stdio servers with `server/discover`, attaches MCP
  version/client/capability metadata to every request, retries a mutually
  supported version on unsupported-version errors, and falls back to the legacy
  initialize handshake only when `server/discover` is not implemented.
- In RC HTTP mode Harn sends `MCP-Protocol-Version`, `Mcp-Method`, and
  `Mcp-Name` where required, does not require `MCP-Session-Id`, mirrors
  `x-mcp-header` tool-schema annotations into `Mcp-Param-*` headers for
  `tools/call`, and handles `input_required` tool results by resolving roots,
  elicitation, and sampling requests before retrying the call.
- If the tool reports `isError: true`, `mcp_call` throws the error text.
- `mcp_connect` throws if the command cannot be spawned or the initialize
  handshake or RC discovery probe fails.

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
| `protocol_mode` | string | Optional MCP client profile: `legacy` (default) or `rc` |

The connected clients are available as properties on the `mcp` global dict:

```harn
pipeline default() {
  let tools = mcp_list_tools(mcp.filesystem)
  log(tools)

  let result = mcp_call(mcp.github, "list_issues", {repo: "harn"})
  log(result)
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
or Streamable HTTP using the MCP protocol, making them callable by Claude
Desktop, Cursor, or any MCP client.

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
| `tool_registry()` | — | tool registry | Create an empty `{_type: "tool_registry", tools: []}` registry |
| `tool_define(registry, name, desc, config)` | registry, name, desc: string, config: dict | dict | Add a tool (config: `{parameters, handler, returns?, annotations?, ...}`) |
| `tool_define_many(registry, specs)` | registry: `ToolRegistry`, specs: `list<ToolDefinitionSpec>` | `ToolRegistry` | Stdlib helper from `std/tools`; add many declarative tool specs to a registry |
| `tool_registry_from(specs)` | specs: `list<ToolDefinitionSpec>` | `ToolRegistry` | Stdlib helper from `std/tools`; create a registry from declarative tool specs |
| `tool_synthesize(config)` | config: dict | closure | Synthesize a deterministic callable tool from a natural-language description |
| `tool_synthesis_cache()` | — | list | Inspect pinned synthesized tool specs for the current run |
| `tool_synthesis_clear()` | — | nil | Clear the current run's synthesized tool cache |
| `composition_binding_manifest(tools, options?)` | tools: list or dict, options?: dict | dict | Build a stable Code Mode binding manifest from Harn, host bridge, MCP, provider-native, or deferred tools |
| `composition_execute(snippet, manifest, options?)` | snippet: string, manifest: dict, options?: dict | dict | Execute a read-only Harn composition snippet and return parent/child audit data |
| `composition_search_examples(query?, limit?)` | query?: string, limit?: int | list | Return curated read-only composition examples |
| `composition_harn_api(manifest)` | manifest: dict | string | Emit typed Harn wrapper declarations for a Code Mode binding manifest |
| `composition_typescript_declarations(manifest)` | manifest: dict | string | Emit declaration-only TypeScript bindings from the manifest |
| `composition_crystallization_trace(report, options?)` | report: dict, options?: dict | dict | Convert a composition report into crystallization trace input |
| `mcp_tools(registry)` | registry: dict | nil | Register tools for MCP serving |
| `mcp_resource(config)` | config: dict | nil | Register a static resource (`{uri, name, text, description?, mime_type?}`) |
| `mcp_resource_template(config)` | config: dict | nil | Register a resource template (`{uri_template, name, handler, description?, mime_type?, completions?}`); `completions` maps URI variable names to static suggestion lists or completion closures |
| `mcp_prompt(config)` | config: dict | nil | Register a prompt (`{name, handler, description?, arguments?}`); prompt arguments may include `suggestions`/`completions` or a `complete` closure for MCP `completion/complete` |
| `mcp_report_progress(progress, opts?)` | progress: number, opts?: dict | bool | Emit a `notifications/progress` update for the in-flight MCP tool call (no-op when the client did not opt in via `_meta.progressToken`). `opts`: `{total?: number, message?: string, token?: string\|number}` |

The `composition_*` builtins back [Governed Code Mode](./code-mode.md). The
executor is read-only: it rejects imports, writes, process execution, network
access, direct HITL, parallel/spawn, and calls outside the manifest bindings and
pure data helpers. Composition snippets may use `map_bounded(...)` for settled
fan-out; child binding calls still honor global and per-MCP-server concurrency
caps, retry policy, trusted MCP annotation gates, idempotency keys, and
`outputSchema` validation. `std/composition` adds MCP-focused helpers including
`composition_mcp_api(...)`, `composition_mcp_execute(...)`, and
`composition_mcp_tools(...)`, which registers the compact MCP profile tools
`harn.code.search_examples`, `harn.code.generate_harn_api`, and
`harn.code.execute_composition`. The MCP executor profile returns only a
reduced result envelope by default; use `composition_execute(...)` directly for
the full child audit report.

`tool_synthesize(config)` is the guarded natural-language tool bootstrapper. It
returns a callable closure and pins the synthesis in an in-memory cache keyed by
a deterministic hash of the normalized description, schemas, capabilities, and
executor. By default the synthesized tool is `executor: "dry_run"`: calling it
validates required arguments and returns a structured preview, but does not hit
external systems.

```harn
let calendar = tool_synthesize({
  description: "fetch the user's calendar for a given date range",
  name: "calendar_fetch",
  parameters: {
    start: {type: "string"},
    end: {type: "string"},
  },
  return_type: {type: "object", properties: {events: {type: "array"}}},
  capabilities: ["http", "oauth:google"],
})

let preview = calendar({start: "2026-04-29", end: "2026-05-06"})
```

To dispatch for real, the synthesis must declare an explicit backend:
`executor: "host_bridge"` with `host_tool`, or `executor: "mcp_server"` with
`mcp_tool` and a connected `mcp_client`. Host and MCP calls still pass through
the active execution policy. `tool_synthesize` does not accept arbitrary
generated Harn handlers; use `tool_define` when the executable body is known and
reviewed.

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
    rules: [
      {deny: {path: "**/.env*"}, reason: "credential file"},
      {ask: {tool: "edit_*", path: "src/**"}, reason: "workspace edit"},
      {deny: {tool: "run_command", command: ["*curl*", "*wget*"]}}
    ],
    auto_approve: ["read*", "list_*"],
    auto_deny: ["shell*"],
    require_approval: ["edit_*", "write_*"],
    write_path_allowlist: ["/workspace/**"],
    repeat_limit: 3
  }
})
```

`rules` is the typed policy DSL. Each rule uses `allow`, `ask`, or `deny`
shorthand, or `{action: "ask", match: {...}}`. Match fields are ANDed inside a
rule and accept strings or string lists:

| Field | Matches |
|---|---|
| `tool` | Tool-name glob |
| `tool_kind` | Annotated ACP tool kind (`read`, `edit`, `execute`, `fetch`, ...) |
| `side_effect` | Annotated side-effect level (`read_only`, `workspace_write`, `process_exec`, `network`) |
| `path` | Declared path arguments from `ToolAnnotations.arg_schema.path_params` |
| `command` / `command_identity` | Shell text or normalized argv/program identity |
| `url` / `domain` / `method` | URL strings, normalized host domains, and HTTP methods found in tool args |
| `mcp_server` / `mcp_tool` | MCP owner inferred from `<server>__<tool>` names or MCP arg metadata |
| `agent` / `persona` / `mode` | Agent/persona/mode identity from args or trigger context |
| `capability` | Annotated capability operation such as `workspace.read_text` |
| `repeat_count_gte` | Same `(session, tool, args)` call count threshold |

Deny beats ask, and ask beats allow regardless of rule order. Legacy
`auto_deny`, `require_approval`, and `auto_approve` are evaluated through the
same precedence model, so old policy bags remain valid while the richer DSL is
available. Tools that match no pattern default to `AutoApproved`.

When an `approval_policy` is active, Harn denies sensitive path strings such as
`.env`, private keys, and credential files by default. Declared host-absolute
paths outside the workspace are also denied unless `external_roots` explicitly
allows the root or `allow_external_paths: true` is set. Set
`allow_sensitive_paths: true` only when a host already mediates secret reads.

`ask` and `require_approval` call the host via the canonical ACP
`session/request_permission` request and **fail closed** if the host does not
implement it. The prompt payload includes a `policyDecision` receipt with the
matched rule, risk labels, normalized context, and rationale. Permission grant
and denial transcript events carry the same receipt under
`metadata.policy_decision` for replay and audit.

Example profiles:

```harn
let local_dev_policy = {
  rules: [
    {allow: {tool_kind: ["read", "search"]}},
    {ask: {tool_kind: ["edit", "move"], path: "src/**"}, reason: "workspace mutation"},
    {deny: {command: ["*curl*", "*wget*"]}, reason: "downloaded shell is not allowed"}
  ],
  repeat_limit: 3
}

let ci_headless_policy = {
  rules: [
    {allow: {tool_kind: ["read", "search"]}},
    {allow: {tool: "run_command", command_identity: ["cargo", "npm", "make"]}},
    {deny: "*"}
  ],
  allow_sensitive_paths: false,
  repeat_limit: 1,
  repeat_action: "deny"
}

let managed_enterprise_policy = {
  rules: [
    {ask: {side_effect: ["workspace_write", "process_exec", "network"]}, reason: "managed approval"},
    {deny: {domain: ["*.pastebin.com", "*.ngrok.io"]}},
    {deny: {path: ["**/.env*", "**/.aws/credentials"]}}
  ],
  external_roots: ["/tmp/harn-approved"]
}
```

Policies compose
across nested scopes with most-restrictive intersection: auto-deny and
require-approval take the union, while `auto_approve` and
`write_path_allowlist` take the intersection. Rule lists concatenate and retain
deny/ask/allow precedence; repeat limits keep the smaller threshold.

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
- All `print`/`println` output goes to stderr (stdout is the MCP transport in stdio mode).
- The server supports the `2025-11-25` MCP protocol version over stdio and Streamable HTTP.
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
| `eval_pack_manifest(payload)` | payload: dict | dict | Normalize an eval pack manifest |
| `eval_pack_run(manifest)` | manifest: dict | dict | Evaluate an eval pack manifest |
| `skill_induce(payload)` | payload: dict | dict | Induce replay-gated `SKILL.md` candidates from crystallization traces |
| `persona_eval_ladder_manifest(payload)` | payload: dict | dict | Normalize a persona eval timeout/budget ladder manifest |
| `persona_eval_ladder_run(manifest)` | manifest: dict | dict | Run a persona eval ladder and write per-tier transcript, receipt, and summary artifacts |
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
| `register_tool_hook(config)` | config: dict | nil | Register a pre/post hook for tool calls matching `pattern` (glob). `deny` string blocks matching tools; `max_output` int truncates results; `pre`/`post` closures can return tool actions or reminder effects |
| `clear_tool_hooks()` | none | nil | Remove all registered tool hooks |

### Session lifecycle hooks

`register_session_hook(event, handler)` (or
`register_session_hook(event, pattern, handler)`) wires a callback
into the whole-session turn loop. Events: `session_start`,
`session_end`, `user_prompt_submit`, `pre_compact`, `post_compact`,
`post_turn`, `permission_asked`, `permission_replied`, `file_edited`,
`session_error`, `session_idle`. The handler receives a typed event
dict (`{event, session: {id}, ...}`) and returns:

- `nil` or `true` — allow the operation to proceed.
- `{block: true, reason}` — veto the operation (honoured for
  `user_prompt_submit` and `pre_compact`).
- `{decision: "allow"|"deny", reason}` — short-circuit a
  `permission_asked` decision.

Each invocation is recorded on the active session transcript under
the `hook_call`, `hook_returned`, and `hook_vetoed` event kinds, so
replay tooling reproduces the same control flow.
For slow background context work, return a receipt from
`std/context/maintenance` and let the host-owned queue run the job.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `register_session_hook(event, pattern?, handler)` | event: string, pattern: string?, handler: closure | nil | Register a session-level lifecycle hook |
| `clear_session_hooks()` | none | nil | Remove all registered session-level hooks |
| `notify_file_edited(path, metadata?)` | path: string, metadata: dict? | nil | Explicitly queue a `file_edited` notification; the standard fs builtins (`write_file`, `append_file`, `write_file_bytes`) also queue automatically. Hooks fire at the next agent-loop turn boundary. |

### Reminder providers

`agent_loop(...)` enables canonical reminder providers by default.
Use `reminders: false` to disable all providers or
`reminders: {providers: ["-token_pressure"]}` to opt out by provider id.
Bare `llm_call(...)` does not fire reminder providers.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `register_reminder_provider(config)` | config: dict | nil | Register a Harn-defined reminder provider. `config.id` is a string, `config.subscribes_to` is an event string or list, and `config.evaluate(ctx)` returns a reminder effect/spec/list or `nil`. |
| `clear_reminder_providers()` | none | nil | Remove user-defined reminder providers. Canonical stdlib providers remain available through `agent_loop` unless disabled with `reminders`. |

### Context and compaction utilities

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `assemble_context(options)` | options: dict | dict | Pack artifacts into a token-budgeted slice of chunks with pluggable ranking, cross-artifact dedup, microcompact chunking, and per-chunk observability. See [Adaptive context assembly](#adaptive-context-assembly) below |
| `estimate_tokens(messages)` | messages: list | int | Estimate token count for a message list (chars / 4 heuristic) |
| `microcompact(text, max_chars?)` | text, max_chars (default 20000) | string | Snip oversized text, keeping head and tail with a marker |
| `select_artifacts_adaptive(artifacts, policy)` | artifacts: list, policy: dict | list | Deduplicate, microcompact oversized artifacts, then select with token budget |
| `transcript_auto_compact(messages, options?)` | messages: list, options: dict | list | Run the same transcript auto-compaction pipeline used by `agent_loop` |

`std/context` also provides the host-neutral
`harn.context_artifact.v1` envelope for durable repository context. Hosts use
`context_artifact(...)` to normalize kind, scope/path, language, role/task
applicability, freshness, confidence, provenance, source hashes, token
estimate, body text, and redaction/sensitivity metadata. The companion helpers
`context_artifact_rank`, `context_artifact_dedupe`, `context_artifact_merge`,
`context_artifact_budget`, and `context_artifact_select` implement the portable
rank/dedupe/merge/budget pass before a host feeds selected artifacts into
`assemble_context` or directly into a prompt.

`context_render_artifacts(...)` renders the same envelope as Markdown, XML,
plain text, or compact lines. With `variant: "auto"`, it uses the same provider
capability flags as logical prompt sections: `prefers_xml_scaffolding` selects
XML and `prefers_markdown_scaffolding` selects Markdown. Existing Burin
`.burin/context-digests` markdown files can be wrapped in-place with
`context_artifact_from_burin_digest(path, body, options?)` during migration.

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
| `sub_agent_request(task, options?)` | task: string, options: dict | dict | Build the normalized child-agent request that `sub_agent_run` sends to the host execution primitive |
| `sub_agent_run(task, options?)` | task: string, options: dict | dict | Run an isolated child agent loop and return a clean envelope `{ok, summary, artifacts, evidence_added, tokens_used, budget_exceeded, data, error, session_id, transcript}`; with `background: true`, returns an agent-handle summary |
| `agent_lifecycle_tools(registry?, options?)` | registry: dict or nil, options: dict or nil | dict | Add model-facing lifecycle tools. Always registers `agent_await_resumption`; registers `subagent_pause` and `subagent_resume` when `{subagents: true}` / `{subagent_tools: true}` is set or the registry already exposes a subagent tool |
| `send_input(handle, task)` | handle, task | dict | Re-run a completed worker with a new task, carrying forward worker state where applicable |
| `suspend_agent(worker, reason?, options?)` | worker, reason, options | dict | Cooperatively suspend a worker, persist a resumable snapshot, and return `status: "suspended"` with `suspension` metadata |
| `resume_agent(worker_or_snapshot, resume_input?, continue_transcript?)` | worker or snapshot, input, bool | dict | Resume a suspended worker, optionally with new input; set `continue_transcript=false` to resume from the prior summary plus new input only |
| `parse_resume_conditions(conditions?)` | conditions | dict or nil | Validate and normalize `ResumeConditions` for self-parking agents and `spawn_agent({options: {resume_when}})` using the trigger-spec validator |
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
It builds a Harn-owned `sub_agent_request(...)`, starts a child session through
the host execution primitive, runs a full `agent_loop`, and returns only a
single typed envelope to the parent:

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
- `reminder_propagation: [...]` to explicitly seed inherited system reminders;
  when omitted, pending parent reminders are filtered by their `propagate`
  policy and inherited automatically
- `response_format: "json"` to parse structured child JSON into `data` from the
  final successful transcript when possible
- `returns: {schema: ...}` to validate that structured child JSON against a
  schema

### Artifacts and context

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `artifact(payload)` | payload: dict | artifact | Normalize a typed artifact/resource |
| `artifact_emit(kind, spec, options?)` | kind, spec, options | dict | Validate and emit a renderable Vega-Lite, Mermaid, or table artifact event |
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
| `agent_session_snapshot(id)` | id | dict or nil | Read-only transcript snapshot plus `length`, `created_at`, `system_prompt`, `tool_format`, `scratchpad`, `scratchpad_version`, `parent_id`, `child_ids`, and `branched_at_event_index` |
| `agent_session_ancestry(id)` | id | dict or nil | Returns `{parent_id, child_ids, root_id}` for the current in-VM lineage |
| `agent_session_reset(id)` | id | nil | Wipes history; preserves id and subscribers |
| `agent_session_fork(src, dst?)` | src, dst | string | Copies transcript, sets `dst.parent_id`, and appends `dst` to `src.child_ids` |
| `agent_session_fork_at(src, keep_first, dst?)` | src, keep_first: int, dst | string | Forks then keeps the first `keep_first` messages on the child; records `branched_at_event_index` |
| `agent_session_scratchpad(id)` | id | dict or nil | Returns the small session-local agent scratchpad |
| `agent_session_set_scratchpad(id, scratchpad, opts?)` | id, scratchpad: dict, opts: dict | dict | Stores a dict scratchpad and returns `{ok, version, scratchpad}`. `opts` may include `source`, `reason`, and `metadata` |
| `agent_session_clear_scratchpad(id, opts?)` | id, opts: dict | dict | Clears the scratchpad and returns `{ok, version, scratchpad: nil}` |
| `agent_session_trim(id, keep_last)` | id, keep_last: int | int | Retain last `keep_last` messages; returns kept count |
| `agent_session_compact(id, opts)` | id, opts: dict | int | Runs the LLM/truncate/observation-mask/custom compactor; custom strategies use `custom_compactor`, `mask_callback`, or `compress_callback` closures |
| `agent_session_inject(id, message)` | id, message: dict | nil | Appends `{role, content, …}`; missing `role` errors |
| `agent_session_seed_from_jsonl(jsonl_path, opts?)` | jsonl_path: string, opts: dict | dict | Create a new session from a replayable `llm_transcript.jsonl` sidecar. Options: `truncate_to_last`, `drop_tool_calls`, `rename_session`, `validate`, `provider`, `model` |
| `agent_session_close(id, status?)` | id, optional status string/dict | nil | Evicts immediately regardless of LRU cap and records an `agent_session_closed` event with the close reason |

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
| `transcript_reminder_event(reminder)` | reminder: dict | event dict | Build a normalized `system_reminder` event (see [System reminders](./system-reminders.md)). Reminder injection and clearing live in [Transcript helpers](#transcript-helpers) above. |
| `transcript_suspension_event(suspension)` | suspension: dict | event dict | Build a normalized `suspension` lifecycle event |
| `transcript_resumption_event(resumption)` | resumption: dict | event dict | Build a normalized `resumption` lifecycle event |
| `transcript_drain_decision_event(drain)` | drain: dict | event dict | Build a normalized `drain_decision` lifecycle event |
| `transcript_stats(transcript)` | transcript | dict | Count messages, tool calls, and visible events on a transcript |
| `transcript_summary(transcript)` | transcript | string or nil | Return transcript summary |
| `transcript_fork(transcript, options?)` | transcript, options | transcript | Fork transcript state |
| `transcript_reset(options?)` | options | transcript | Start a fresh active transcript with optional metadata |
| `transcript_archive(transcript)` | transcript | transcript | Mark transcript archived and append an internal lifecycle event |
| `transcript_abandon(transcript)` | transcript | transcript | Mark transcript abandoned and append an internal lifecycle event |
| `transcript_resume(transcript)` | transcript | transcript | Mark transcript active again and append an internal lifecycle event |
| `transcript_compact(transcript, options?)` | transcript, options | transcript | Compact a transcript with the runtime compaction engine, including reminder TTL/dedupe/preserve handling and `strategy: "custom"` plus `custom_compactor(messages, reminders)` |
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
