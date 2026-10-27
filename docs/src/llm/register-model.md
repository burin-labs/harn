# Register a model

Add one model registration in `crates/harn-vm/src/llm/catalog_sources/60-models/`,
then generate the runtime catalog, capabilities, bindings, and provider documentation:

```sh
make gen-provider-catalog
make check-provider-catalog check-provider-matrix check-provider-support
```

Use the owning provider's live API to verify tools, tool results, reasoning controls,
structured output, streaming, and any other capability the registration claims.
Record failures as well as successes. A successful HTTP response does not prove
that a provider honored a parameter.

## Write one source entry

Keep ordinary model facts in the model table. Place request policy, aliases,
gateway routes, and picker placement beneath its source-only `registration`:

```toml
[models."example-current"]
name = "Example current"
provider = "example"
context_window = 200000
operations = ["text_generation"]
capabilities = ["tools", "streaming"]

[models."example-current".registration]
aliases = ["current"]

[models."example-current".registration.request]
native_tools = true
preferred_tool_format = "native"

[models."example-current".registration.routes."gateway/example/current"]
provider = "gateway"
model = { name = "Example current via gateway", wire_model = "example/current" }
request = { reasoning_none_supported = false }
```

The generator derives exact request matches from the catalog and wire IDs.
Do not author `model_match` or `extends` inside a registration.
Route requests inherit the base request fields and override individual fields.
Route model fields also inherit the base; each override replaces the entire field,
including a complete pricing card or serving-tier list. Clear direct-only modes
with `serving_tiers = []` and `reasoning_modes = []` on gateway routes.
Provider names must already exist in the catalog.

For an existing model family, add `registration.selection` with `family`,
`dimension`, optional `before`, and a `value` containing its label and tradeoffs.
The dimension must have kind `model`; the generator supplies `model_id`.
Use `registration.supersedes` or a route's `supersedes` to name existing model
rows that this registration replaces. Remove any separately authored replacement
pointer so the replacement has one owner.

Unknown registration or request fields, duplicate aliases/routes, and missing
picker dimensions fail generation. Runtime artifacts contain no `registration`
metadata. Installed runtimes consume the generated typed catalog.

## Reuse measured facts

Use [Models.dev bindings](models-dev-source.md) for suitable pinned facts.
Retain complete local pricing cards when upstream data cannot express their
conditions. Keep measured transport behavior in the registration.
Catalog generation and its checks read source without rebuilding the CLI;
rebuild before exercising a runtime whose embedded catalog changed.
