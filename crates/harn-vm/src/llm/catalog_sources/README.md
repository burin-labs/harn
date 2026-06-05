# Provider Catalog Sources

These TOML fragments are the editable source for Harn's bundled provider/model
catalog. The runtime embeds `../providers.toml`, which is generated from this
directory by:

```sh
harn providers build-config
harn providers export
```

Use `harn providers build-config --check` to verify that `../providers.toml`
matches the fragments. Direct edits to `../providers.toml` are invalid because
the next generation pass will overwrite them.

Fragments use the same `ProvidersConfig` schema as user overrides and package
manifest `[llm]` sections. File names are sorted lexicographically, so keep
numeric prefixes when adding a fragment that depends on earlier table context
or routing order.

Local runtime lifecycle facts live under
`[providers.<id>.local_runtime]`. Keep provider mechanics there: launch kind,
command, default arg names, stop strategy, provenance URL, and freshness date.
Do not put machine-specific absolute model paths in the bundled catalog; use
`model_source_env`, CLI `--model-source`, or a user/project provider overlay.

Model-specific local sizing facts live under `[models.<id>.local_memory]`.
Keep the shape simple and empirical: measured resident GiB, measured context,
default cache type, base resident GiB, KV-cache GiB per 1K context, cache-type
multipliers, safety margin, freshness date, and notes. These rows are launch
guards and recommendations, not exact runtime allocator models; add them only
when the route has been measured or a trustworthy source documents the numbers.
