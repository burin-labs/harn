# Prompt templates: v2 migration

The prompt-template engine used by `render(...)` / `render_prompt(...)` now
supports `else`/`elif`, loops, includes, filters, comments, raw blocks, and
whitespace trim markers. Existing templates keep rendering unchanged — this
is a strict superset. But many pre-v2 workarounds can now be simplified.

## If / else

**Before** — mutually-exclusive `{{ if }}` blocks with inverted flags:

```text
{{if expected_output}}
Expected: {{expected_output}}
{{end}}{{if no_expected_output}}
(no expected output provided)
{{end}}
```

**After**:

```text
{{if expected_output}}
Expected: {{expected_output}}
{{else}}
(no expected output provided)
{{end}}
```

## Loops instead of hand-rolled list concatenation

**Before** — build a string in `.harn` and inject it as a single variable:

```harn,ignore
let block = ""
for sample in samples {
  block = "${block}### ${sample.path}\n\`\`\`\n${sample.content}\n\`\`\`\n\n"
}
let prompt = render("enrichment.prompt", {block: block, ...})
```

```text
# enrichment.prompt
## Samples
{{block}}
```

**After** — iterate in the template:

```harn,ignore
let prompt = render("enrichment.prompt", {samples: samples, ...})
```

````text
# enrichment.prompt
## Samples
{{for s in samples}}
### {{s.path}}
```
{{s.content}}
```
{{end}}
````

## Shared prose → `{{ include }}`

When multiple repair-stage prompts share the same boilerplate
("self-verification instructions", system rules, etc.), extract the shared
text into a partial:

```text
# lib/partials/self-verify.harn.prompt
Before responding, verify your answer against: {{verification_hint}}
```

Call it from each repair stage:

```text
{{include "partials/self-verify.harn.prompt"}}
...stage-specific instructions...
```

Pass stage-specific overrides with `with`:

```text
{{include "partials/self-verify.harn.prompt" with { verification_hint: "compile output" }}}
```

## Filters instead of pre-processing

**Before** — uppercase, join lists, JSON-stringify in `.harn` before rendering:

```harn,ignore
let tags_str = join(map(tags, fn(t) { return uppercase(t) }), ", ")
render("x.prompt", {tags: tags_str})
```

**After**:

```text
Tags: {{tags | join: ", " | upper}}
```

## Comments and raw blocks

Add `{{# authoring notes #}}` to document a template without leaking the note
into the final prompt. Wrap literal `{{` / `}}` (e.g. examples of another
template language embedded in a prompt) in a `{{ raw }} ... {{ endraw }}`
block.

## Whitespace trim

`{{- ... -}}` markers strip whitespace and one newline on the respective
side. Use them to keep source templates readable without introducing blank
lines in the rendered output:

```text
Items:
{{- for x in xs -}}
  {{ x }},
{{- end -}}
DONE
```

See [Prompt templating](../prompt-templating.md) for the full reference.
