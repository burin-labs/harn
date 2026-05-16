# Prompt templating

Harn ships a small template language for rendering `.harn.prompt` and `.prompt`
asset files or inline template strings. It is invoked by the
`render(path, bindings?)`, `render_prompt(path, bindings?)`, and
`render_string(template, bindings?)` builtins (and, equivalently, via the
`template.render` host capability). The engine is intentionally minimal — a
rendering layer for prompts, not a scripting language — but it covers the
ergonomics most prompt authors reach for: conditionals with `else`/`elif`,
loops, includes, filters, comments, and whitespace control.

Use `render_string(...)` when the template should live inline in source. Use
`render(...)` / `render_prompt(...)` when the template should be loaded from a
separate file relative to the calling module. The template syntax and error
shape are identical across both entrypoints.

Stdlib-owned model-facing prompt prose lives in embedded `std/...harn.prompt`
assets. These render without touching the filesystem and report stable
`std://...` provenance URIs:

```harn,ignore
let tool_contract = render_prompt("std/agent/prompts/tool_contract_text.harn.prompt", {})
```

Protected Rust orchestration paths are checked by `make lint-no-rust-prompt-prose`;
move reusable model-facing instructions into stdlib prompt assets unless the
Rust literal is a short diagnostic, parser/protocol token, provider API string,
or other primitive constant.

This page is the reference. The one-page [quickref](./docs/llm/harn-quickref.md)
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
{{ section "task" }} ... {{ endsection }}
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

Embedded stdlib prompts can include other embedded stdlib prompts with either
relative paths or explicit `std/...` paths. Relative stdlib includes stay inside
the embedded stdlib prompt catalog rather than escaping to the filesystem.

The included template inherits the parent's scope by default. Pass explicit
bindings with `with { ... }` — these are merged into the parent scope for the
inner render only:

```text
{{ include "partials/item.prompt" with { item: x, style: "bold" } }}
```

### Package-root paths

Refactor-safe alternatives to `../../partials/foo.harn.prompt`:

```text
{{ include "@/prompts/header.harn.prompt" }}
{{ include "@partials/header.harn.prompt" }}
```

- `@/<rel>` — anchored at the calling file's project root (nearest
  `harn.toml` ancestor).
- `@<alias>/<rel>` — anchored at an entry in the project's
  `[asset_roots]` table:

  ```toml
  # harn.toml
  [asset_roots]
  partials = "src/prompts/partials"
  ```

These forms work in both the runtime template engine and the
`render(...)` / `render_prompt(...)` builtins. `harn check` validates
them against `harn.toml`. See
[modules.md](./modules.md#package-root-prompt-assets) for the full
reference.

**Safety:**

- Circular includes are detected (e.g. `a.prompt` includes `b.prompt` which
  includes `a.prompt`) and produce a `circular include detected` error with
  the full chain.
- Include depth is capped at 32 levels.
- A missing included file fails with `failed to read included template <path>`.
- `@/...` and `@<alias>/...` reject `..` segments and absolute targets;
  package-rooted assets cannot escape the project root.

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
- `unknown template section \`foo\`` — `{{ section "foo" }}` is not one of
  the built-in logical sections.
- ``{{ section }}` missing matching `{{ endsection }}`` — a logical section
  was not closed.

## The `llm` scope

When `render()` / `render_prompt()` / `render_string()` is invoked from
inside an LLM-aware frame (`llm_call`, the default handler stack, or
`agent_loop`), the engine auto-injects a reserved `llm` binding so a
single logical template can adapt its wire envelope per provider
without manual option threading:

```harn-prompt
{{ if llm.capabilities.native_tools }}
  Call `finish_task` when done.
{{ else }}
  When done, output: `<<DONE>>`
{{ end }}
```

The injected value is shaped:

```text
llm = {
  provider: "anthropic",          // resolved provider name
  model:    "claude-3-5-sonnet",  // resolved model id
  family:   "claude",             // canonical model-family token
  capabilities: { ... },          // result of provider_capabilities()
}
```

`family` is one of `claude` / `gpt` / `gemini` / `qwen` / `llama` /
`mistral` / `deepseek` / `phi` / `grok` / `command`, derived from the
model id with a provider-alias fallback. Branch on `family` for
human-readable identity checks; branch on `capabilities.*` for wire-
format adaptation (native vs text-format tools, prompt caching,
structured output, etc.).

Bare `render()` calls outside any LLM frame leave `llm = nil`, so the
same template works in CI / doc-gen contexts as long as it guards with
`{{ if llm }}`:

```harn-prompt
{{ if llm }}
  Targeting {{ llm.provider }}/{{ llm.model }} — using
  {{ if llm.capabilities.native_tools }}native{{ else }}text{{ end }} tools.
{{ else }}
  Generic prompt — no model selected.
{{ end }}
```

The frame is published by:

- the native `llm_call` builtin for its full call duration (covers
  middleware-driven re-renders during schema retry),
- `default_llm_caller` for the entire handler-stack closure (so
  user-authored middleware that renders sees the same context), and
- `agent_loop` for the whole loop, so every per-turn system-prompt
  partial (loop contract, native-tool contract, skill catalog, …)
  renders against the active provider/model.

User bindings that already supply an `llm` key are left untouched —
the engine emits a one-shot lint warning under the
`template.llm_scope` category and the user's value wins for
back-compat. Rename your key (e.g. `local_model`, `current_llm`) to
clear the warning.

## Logical sections

Logical sections encode the role of a prompt chunk and let the renderer choose
the wire envelope from `llm.capabilities`:

```harn-prompt
{{ section "task" }}
Build a CLI that prints a directory tree.
{{ endsection }}

{{ section "output_format" schema=output_schema }}{{ endsection }}

{{ section "tools" tools=tool_list }}{{ endsection }}
```

The built-in section names are:

| Section | Purpose |
| ------- | ------- |
| `task` | Primary user task or instruction. |
| `examples` | Few-shot examples or demonstrations. |
| `output_format` | Output schema or parse contract. Suppressed when `structured_output_mode == "native_json"` because the provider envelope carries the schema. |
| `tools` | Tool definitions rendered as XML, Markdown/JSON, or a ReAct-style text contract depending on the route. |
| `thinking_scaffold` | Thinking envelope for models that need one; no output for models with no thinking section style. |
| `chain_of_thought` | A provider-shaped private-reasoning instruction. |
| `system_framing` | Top-level persona or policy framing; uses developer-role wording when `prefers_role_developer` is true. |

Section arguments use `name=value` pairs. `output_format` accepts `schema=...`;
`tools` accepts `tools=...`. Section bodies can contain normal template
directives, including `if`, `for`, and `include`.

```harn-prompt
{{ section "examples" }}
{{ for ex in examples }}
- Input: {{ ex.input }}
- Output: {{ ex.output }}
{{ end }}
{{ endsection }}
```

Capability dispatch is feature-based, not provider-string-based:

| Capability | Effect |
| ---------- | ------ |
| `prefers_xml_scaffolding` | `task`, `examples`, and `system_framing` render as XML tags. |
| `prefers_markdown_scaffolding` | Those sections render as Markdown headings. |
| `prefers_xml_tools` | `tools` renders as `<tools>...</tools>`. |
| `native_tools` | `tools` renders as a Markdown/JSON description when XML tools are not preferred. |
| `text_tool_wire_format_supported` | `tools` renders a ReAct-style text-tool contract for local/text routes. |
| `structured_output_mode` | Controls `output_format`; `native_json` omits the section. |
| `thinking_block_style` | Controls `thinking_scaffold` (`none`, `thinking_blocks`, `reasoning_summary`, `inline`). |
| `prefers_role_developer` | `system_framing` targets developer instructions instead of system instructions. |

## Variant resolution in transcripts

Every `render()` / `render_prompt()` call made under an LLM-aware frame
(`llm_call`, `default_llm_caller`, `agent_loop`) emits a
`template.render` event into the run's `llm_transcript.jsonl`. The event
captures:

- The resolved `llm` snapshot — `provider`, `model`, `family`, and the
  full `capabilities` map at render time.
- A branch trace listing every `{{ if }}` / `{{ elif }}` / `{{ else }}`
  decision and every `{{ section }}` envelope that fired, anchored to
  source line + column.
- The template URI and a stable content hash (`template_revision_hash`)
  so replay can detect "the rendered text matches but the source drifted
  underneath" drift.

The portal renders this as a "Variant resolution" panel in the run
detail view. Renders outside any LLM frame (doc-gen, CI) emit no event.
The trace is deterministic — the same `llm` snapshot and the same
bindings always produce the same trace, which is what makes replay
reproducible.

## Drift-prevention lints

`harn lint` walks `.harn.prompt` (and bare `.prompt`) files alongside
`.harn` programs and enforces two rules that keep the
capability-adaptive primitive honest:

| Rule | What it catches |
| :--- | :--- |
| `template-provider-identity-branch` | Branching directly on `llm.provider`, `llm.model`, or `llm.family`. The diagnostic suggests the corresponding capability flag (e.g. `provider == "anthropic"` → `llm.capabilities.prefers_xml_scaffolding`). |
| `template-variant-explosion` | More than `N` capability-aware conditionals in a single template. Default `N=3`, configurable via `[lint] template_variant_branch_threshold` in `harn.toml`. |

Both rules can be disabled per-file via the standard `[lint] disabled`
list. Lifting capability branches into [logical sections](#logical-sections)
is the recommended way to silence variant-explosion without raising the
threshold — the section's envelope dispatch lives in one shared place
instead of being scattered across the prompt.

## Preflight checks

`harn check` parses every template referenced by a literal `render(...)` /
`render_prompt(...)` call and surfaces syntax errors before you run the
pipeline. `render_string(...)` is available to the checker as a builtin, but
its template body is evaluated at runtime because it comes from normal string
expressions rather than a source-relative asset path. This catches things like
an unterminated `{{ for }}` block at static time for file-backed templates,
rather than at first render.

## Back-compat

The engine is a strict superset of the pre-v2 syntax:

- `{{ name }}` — interpolation, missing bare identifier passes through verbatim
- `{{ if key }} ... {{ end }}` — truthy test

All pre-v2 templates render identically. Migrating awkward workarounds to
the new forms is optional but usually shorter — see the
[migration guide](./migrations/template-engine-v2.md).
