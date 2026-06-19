## Runtime values

| Type | Syntax | Description |
|---|---|---|
| `string` | `"text"` | UTF-8 string |
| `bytes` | builtin-produced | Immutable byte buffer |
| `int` | `42` | Platform-width integer |
| `float` | `3.14` | Double-precision float |
| `decimal` | `decimal("0.10")` | Exact base-10 number (96-bit) for money/precise arithmetic. No literal syntax — constructed via the `decimal()` builtin. Arithmetic promotes `int` exactly but never mixes with `float` (a type error); equality/ordering compare only against `decimal` (scale-insensitive). Excluded from the `number` alias. |
| `number` | `42` / `3.14` | Built-in alias for `int \| float` (does **not** include `decimal`) |
| `bool` | `true` / `false` | Boolean |
| `nil` | `nil` | Null value |
| `list` | `[1, 2, 3]` | Ordered collection |
| `dict` | `{key: value}` | String-keyed map |
| `set` | `set(1, 2, 3)` | Unordered collection of unique values |
| `closure` | `{ x -> x + 1 }` | First-class function with captured environment |
| `enum` | `Color.Red` | Enum variant, optionally with associated data |
| `struct` | `Point({x: 3, y: 4})` | Struct instance with named fields |
| `taskHandle` | (from `spawn`) | Opaque handle to an async task |
| `Generator<T>` | regular `fn` containing `yield` | Existing synchronous generator value |
| `Stream<T>` | `gen fn` containing `emit` | Lazy, single-pass stream value |
| `Iter<T>` | `x.iter()` / `iter(x)` | Lazy, single-pass, fused iterator. See [Iterator protocol](#iterator-protocol) |
| `Pair<K, V>` | `pair(k, v)` | Two-element value; access via `.first` / `.second` |

### Truthiness

| Value | Truthy? |
|---|---|
| `bool(false)` | No |
| `nil` | No |
| `int(0)` | No |
| `float(0)` | No |
| `string("")` | No |
| `bytes(b"")` | No |
| `list([])` | No |
| `dict([:])` | No |
| `set()` (empty) | No |
| Everything else | Yes |

### Equality

Values are equal if they have the same type and same contents, with these exceptions:

- `int` and `float` are compared by converting `int` to `float`
- Two closures are never equal
- Two task handles are equal if their IDs match

### Comparison

Only `int`, `float`, and `string` support ordering (`<`, `>`, `<=`, `>=`).
Comparison between other types returns 0 (equal).

