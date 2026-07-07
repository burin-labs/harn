<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Structs

Structs define named record types with typed fields. Structs may also be
generic.

### Struct declaration

```harn
struct Point {
  x: int
  y: int
}

struct User {
  name: string
  age: int
}

struct Pair<A, B> {
  first: A
  second: B
}
```

Fields are declared with `name: type` syntax, one per line.

### Struct construction

Struct instances can be constructed with the struct name followed by
a named-field body:

```harn
struct Point { x: int, y: int }
struct User { name: string, age: int }
struct Pair<A, B> { first: A, second: B }
const p = Point { x: 3, y: 4 }
const u = User { name: "Alice", age: 30 }
const pair: Pair<int, string> = Pair { first: 1, second: "two" }
```

### Field access

Struct fields are accessed with dot syntax, the same as dict property
access:

```harn
log(p.x)    // 3
log(u.name) // "Alice"
```

