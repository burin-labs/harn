# Migration: package-root prompt assets

Harn supports two refactor-safe forms for addressing `.harn.prompt` assets:

- `@/<rel>` — anchored at the calling file's project root (the nearest
  `harn.toml` ancestor).
- `@<alias>/<rel>` — anchored at a `[asset_roots]` entry in `harn.toml`.

Existing `render(...)` / `render_prompt(...)` calls keep their
source-relative behavior unchanged. Migrating brittle `../../...`
paths to package-root form is optional but stops asset references from
breaking when callers move.

## Before — relative paths break on file moves

```harn,ignore
// pipelines/lib/runtime/workflow/graph-stages.harn
render_prompt("../../../partials/tool-examples.harn.prompt", bindings)
```

A pure refactor that relocates `graph-stages.harn` silently breaks the
asset path with no compile-time signal — `harn check` will not catch it
because the source-relative resolver still produces a path that points
at a non-existent file.

## After — project-root form

```harn,ignore
render_prompt("@/pipelines/partials/tool-examples.harn.prompt", bindings)
```

Verbose but resilient: the path resolves the same regardless of where
the caller lives. `harn check` validates it during preflight.

## Better — `[asset_roots]` alias

Define an alias in the project's `harn.toml`:

```toml
[asset_roots]
partials = "pipelines/partials"
```

Then:

```harn,ignore
render_prompt("@partials/tool-examples.harn.prompt", bindings)
```

Aliases are resolved against the project root, so they work from any
file in the workspace.

## What changed in the runtime

- `render(...)`, `render_prompt(...)`, `render_with_provenance(...)`,
  the `template.render` host capability, and `{{ include "..." }}`
  directives now recognize the `@/...` and `@<alias>/...` forms.
- `harn check` reports a `preflight: ...` diagnostic when:
  - the calling file has no `harn.toml` ancestor;
  - an `@<alias>/...` reference targets an alias that isn't defined in
    `[asset_roots]`;
  - the resolved file does not exist.
- `harn contracts bundle` records every resolved `@`-path under
  `prompt_assets` so packagers don't need to maintain a separate file
  list.
- The Harn LSP go-to-definition jumps from a literal
  `render_prompt("@/...")` argument straight to the target prompt file.

## Safety

Both forms reject `..` segments and absolute targets. A
`render_prompt("@/../escape")` call fails with `invalid project-root
asset path`, so a package-rooted asset cannot reach outside the
project root.

## Related

- Reference: [modules.md](../modules.md#package-root-prompt-assets)
- Templating: [prompt-templating.md](../prompt-templating.md#package-root-paths)
- Spec: `[asset_roots]` table in `spec/HARN_SPEC.md`
