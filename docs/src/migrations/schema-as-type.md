# Migration — schema-as-type (`type` aliases drive `output_schema`)

Prior to this change, Harn had two parallel representations for
structured LLM output:

1. **Harn-native types** — `type Foo = {verdict: string, ...}`.
2. **Raw JSON-Schema dicts** — passed as `output_schema: {type: "dict",
   properties: {...}, required: [...]}` to `llm_call`, and consumed by
   `schema_is`, `schema_expect`, `schema_parse`, and friends.

The two representations drifted. A grader script that declared a type
alias for documentation and a separate schema dict for validation had
no compile-time check that the two agreed.

This release unifies them. A single `type` alias now feeds:

- Static type-checking on the values that flow through it.
- JSON-Schema emission for `llm_call` structured output.
- `schema_is` / `schema_expect` narrowing on runtime-typed values
  (`unknown`, unions, parsed JSON).
- ACP `ToolAnnotations.args` compatibility (same emitted schema).

## Migrating a grader script

Before — duplicated surface, no cross-check:

```harn
let grader_schema = {
  type: "object",
  required: ["verdict", "summary"],
  properties: {
    verdict: {type: "string", enum: ["pass", "fail", "unclear"]},
    summary: {type: "string"},
  },
}

let r = llm_call(prompt, nil, {
  model: routing.model,
  output_schema: grader_schema,
  schema_retries: 2,
})

// No compile-time check that r.data has these shape/fields.
log("verdict=${r.data.verdict}")
```

After — one alias, two uses:

```harn
type GraderOut = {
  verdict: "pass" | "fail" | "unclear",
  summary: string,
}

let r = llm_call(prompt, nil, {
  model: routing.model,
  output_schema: GraderOut,   // compiled to the JSON-Schema dict
  schema_retries: 2,
})

if schema_is(r.data, GraderOut) {
  // r.data is narrowed to GraderOut here.
  log("verdict=${r.data.verdict}")
}
```

## What translates mechanically

| Old schema key | New type grammar |
|---|---|
| `{type: "string"}` | `string` |
| `{type: "int"}` / `"integer"` | `int` |
| `{type: "bool"}` / `"boolean"` | `bool` |
| `{type: "list", items: T}` | `list<T>` |
| `{type: "dict", additional_properties: V}` | `dict<string, V>` |
| `{type: "string", enum: ["a","b"]}` | `"a" \| "b"` |
| `{type: "int", enum: [0,1,2]}` | `0 \| 1 \| 2` |
| `{properties, required}` with `additional_properties: false` | `type T = {field: type, optional?: type}` |
| `{union: [A, B]}` / `{oneOf: [A, B]}` | `A \| B` |
| `{nullable: true}` wrapping `T` | `T \| nil` |

## Staying with raw schema dicts

Nothing forces you to migrate. `output_schema: dict_literal` still
works and is still the right tool when you need schema features Harn's
type grammar does not yet express (regex `pattern`, `min_length`,
numeric `min`/`max`, `const`, nested `$ref`, etc.). You can mix:

```harn
type Name = {first: string, last: string}

let r = llm_call(prompt, nil, {
  output_schema: {
    type: "dict",
    properties: {
      name: schema_of(Name),       // alias → schema dict
      email: {type: "string", pattern: "^[^@]+@[^@]+$"},
    },
    required: ["name", "email"],
  },
})
```

## Caveats

- `schema_of(T)` lowers at compile time. `T` must be a top-level
  `type` alias visible to the compiler. Dynamic construction
  (`let T = ...`) falls back to the runtime `schema_of` builtin, which
  is a dict-passthrough — it does not look up alias names at runtime.
- The compiler-level alias emitter handles shapes, lists,
  `dict<string, V>`, literal-string/int unions, and nested aliases.
  Shapes containing `Applied<T>` (generic containers) or `fn` types
  emit a best-effort `{type: "closure"}` placeholder; prefer raw
  schema dicts there.
- `response.data` of `llm_call(..., {output_schema: T})` is not yet
  automatically narrowed to `T` by the type checker. Use
  `if schema_is(r.data, T) { ... }` in the interim — the narrowing
  there is exact.
