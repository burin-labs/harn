# Prompt templating

Harn ships a small template language for rendering `.harn.prompt` and `.prompt`
asset files. It is invoked by the `render(path, bindings?)` and
`render_prompt(path, bindings?)` builtins (and, equivalently, via the
`template.render` host capability). The engine is intentionally minimal — a
rendering layer for prompts, not a scripting language — but it covers the
ergonomics most prompt authors reach for: conditionals with `else`/`elif`,
loops, includes, filters, comments, and whitespace control.

This page is the reference. The one-page [quickref](./llm/harn-quickref.md)
has a condensed version for agents writing Harn.

## At a glance

```text
{{ name }}                                   interpolation
{{ user.name }} / {{ items[0] }}             nested path access
{{ name | upper | default: "anon" }}         filter pipeline
{{ if expr }} ... {{ elif expr }} ... {{ else }} ... {{ end }}
{{ for item in xs }} ... {{ else }} ... {{ end }}       else = empty-iterable fallback
{{ for key, value in dict }} ... {{ end }}
{{ include "partial.harn.prompt" }}
{{ include "partial.harn.prompt" with { x: name } }}
{{# stripped at parse time #}}
{{ raw }} ... literal {{braces}} ... {{ endraw }}
{{- name -}}                                 whitespace-trim markers
```

## Interpolation

`{{ path }}` evaluates an expression and writes its string form into the
output. Paths support nested field access and integer/string indexing:

```text
{{ user.name }}          — field
{{ user.tags[0] }}       — list index
{{ user.tags[-1] }}      — negative index (counts from end)
{{ config["api-key"] }}  — string key with non-identifier characters
```

Missing values render as the empty string, except for legacy *bare*
identifiers (e.g. `{{ name }}` with no dots/brackets/filters). For
back-compat, those render their source verbatim on a miss (the pre-v2
behavior), so existing templates that relied on "missing → literal passthrough"
keep working.

## Conditionals

```text
{{ if role == "admin" }}
  welcome, admin
{{ elif role == "user" and active }}
  welcome back!
{{ else }}
  please sign in
{{ end }}
```

Only `{{ if expr }} ... {{ end }}` is required; `elif` and `else` branches are
optional and can be combined. The **expression** grammar is:

| Category           | Syntax                                                  |
| ------------------ | ------------------------------------------------------- |
| Literals           | `"str"`, `'str'`, `123`, `1.5`, `true`, `false`, `nil`  |
| Paths              | `ident`, `a.b.c`, `a[0]`, `a["key"]`                    |
| Unary              | `not x`, `!x`                                           |
| Equality           | `==`, `!=`                                              |
| Comparison         | `<`, `<=`, `>`, `>=` (numbers and strings)              |
| Boolean (short-cut)| `and` / `&&`, `or` / `\|\|`                             |
| Grouping           | `(expr)`                                                |
| Filters            | `expr \| filter`, `expr \| filter: arg1, arg2`          |

String escapes inside quoted literals: `\n`, `\t`, `\r`, `\\`, `\"`, `\'`.

### Truthiness

Used both by `if` and by the short-circuit `and`/`or`:

| Value kind                     | Truthy? |
| ------------------------------ | ------- |
| `nil`                          | false   |
| `false`                        | false   |
| `0`, `0.0`                     | false   |
| empty/whitespace-only string   | false   |
| empty list / set / dict        | false   |
| everything else                | true    |

## Loops

```text
{{ for x in xs }}
- {{ loop.index }}. {{ x }}
{{ else }}
(no items)
{{ end }}
```

`{{ else }}` inside a `for` block renders when the iterable is empty — a
cleaner alternative to wrapping the loop in an `{{ if }}`.

### Loop variables

Inside the loop body, a synthetic `loop` dict is in scope:

| Field          | Type   | Description                      |
| -------------- | ------ | -------------------------------- |
| `loop.index`   | int    | 1-based index of the current item|
| `loop.index0`  | int    | 0-based index                    |
| `loop.first`   | bool   | `true` on the first iteration    |
| `loop.last`    | bool   | `true` on the final iteration    |
| `loop.length`  | int    | total number of items            |

### Dict iteration

```text
{{ for key, value in my_dict }}
  {{ key }} = {{ value }}
{{ end }}
```

Dicts iterate in their canonical (BTreeMap) order.

## Includes

Include another template file. Paths resolve relative to the *including*
file's directory:

```text
{{ include "partials/header.harn.prompt" }}
```

The included template inherits the parent's scope by default. Pass explicit
bindings with `with { ... }` — these are merged into the parent scope for the
inner render only:

```text
{{ include "partials/item.prompt" with { item: x, style: "bold" } }}
```

**Safety:**

- Circular includes are detected (e.g. `a.prompt` includes `b.prompt` which
  includes `a.prompt`) and produce a `circular include detected` error with
  the full chain.
- Include depth is capped at 32 levels.
- A missing included file fails with `failed to read included template <path>`.

## Comments

```text
Before{{# this never renders #}}After
```

Comments are stripped entirely at parse time. Use them to document a template
without leaking the note into the final prompt.

## Raw blocks

When a prompt needs to emit literal `{{` / `}}` (say, the prompt includes
another template language, JSON with braces, etc.):

```text
{{ raw }}
{{ this is output verbatim }}
{{ endraw }}
```

Everything between `{{ raw }}` and `{{ endraw }}` is passed through as-is,
no directive interpretation.

## Whitespace control

Directives support `{{- ... -}}` trim markers (Jinja-style). A leading `-`
strips the preceding whitespace *and one newline*; a trailing `-` strips the
following whitespace and one newline. This is the idiomatic way to keep
templates readable without emitting extra blank lines:

```text
Items:
{{- for x in xs -}}
  {{ x }},
{{- end -}}
DONE
```

renders `Items:  a,  b,  c,DONE` — no leading or trailing newlines introduced
by the control directives.

## Filters

Apply transformations to a value via a pipeline. Filters can be chained and
some accept arguments after a colon:

```text
{{ items | join: ", " }}
{{ name | upper }}
{{ user.bio | default: "(no bio)" | indent: 4 }}
```

### Built-in filters

| Filter                  | Args                  | Description                                                        |
| ----------------------- | --------------------- | ------------------------------------------------------------------ |
| `upper`                 | —                     | Uppercase the string form                                          |
| `lower`                 | —                     | Lowercase                                                          |
| `trim`                  | —                     | Strip leading/trailing whitespace                                  |
| `capitalize`            | —                     | First char upper, rest lower                                       |
| `title`                 | —                     | Title Case (uppercase each word)                                   |
| `length`                | —                     | Number of items (string chars, list/set/dict entries, range size)  |
| `first`                 | —                     | First element (or char)                                            |
| `last`                  | —                     | Last element (or char)                                             |
| `reverse`               | —                     | Reversed list or string                                            |
| `join`                  | `sep: string`         | Join list items with `sep`                                         |
| `default`               | `fallback: any`       | Use `fallback` when the value is falsey                            |
| `json`                  | `pretty?: bool`       | Serialize as JSON (pass `true` for pretty)                         |
| `indent`                | `width: int, first?: bool` | Indent every line by `width` spaces; pass `true` to indent the first line too |
| `lines`                 | —                     | Split string on `\n` into a list                                   |
| `escape_md`             | —                     | Escape Markdown special characters                                 |
| `replace`               | `from: str, to: str`  | Replace all occurrences                                            |

Unknown filters raise a clear error at render time.

## Errors

On any parse or render error, the engine raises a thrown value (via
`VmError::Thrown`) with a message of the form:

```text
<template-path> at <line>:<col>: <what went wrong>
```

Typical cases:

- `unterminated directive` — a `{{` without a matching `}}`.
- `unterminated comment` — a `{{#` without a matching `#}}`.
- `unterminated \`{{ raw }}\` block` — missing `{{ endraw }}`.
- `unknown filter \`foo\`` — the named filter isn't registered.
- `circular include detected: a.prompt → b.prompt → a.prompt`.
- `include path must be a string` — `{{ include }}` target wasn't a string.

## Preflight checks

`harn check` parses every template referenced by a literal `render(...)` /
`render_prompt(...)` call and surfaces syntax errors before you run the
pipeline. Catches things like an unterminated `{{ for }}` block at static
time rather than at first render.

## Back-compat

The engine is a strict superset of the pre-v2 syntax:

- `{{ name }}` — interpolation, missing bare identifier passes through verbatim
- `{{ if key }} ... {{ end }}` — truthy test

All pre-v2 templates render identically. Migrating awkward workarounds to
the new forms is optional but usually shorter — see the
[migration guide](./migrations/template-engine-v2.md).
