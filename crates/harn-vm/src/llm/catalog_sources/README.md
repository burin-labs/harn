# Provider Catalog Sources

These TOML fragments are the editable source for Harn's bundled provider/model
catalog. The runtime embeds `../providers.toml`, which is generated from this
directory by:

```sh
harn provider catalog generate
```

Use `harn provider catalog generate --check` to verify that `../providers.toml`
and every checked-in provider catalog projection match the fragments. Direct
edits to `../providers.toml` are invalid because the next generation pass will
overwrite them.

## Rebuild between `generate` and `matrix`

`generate` and `matrix` do not read the same input, so one build is not enough:

```sh
cargo build -p harn-cli --bin harn     # 1. build
harn provider catalog generate         # 2. reads THESE fragments off disk
cargo build -p harn-cli --bin harn     # 3. build AGAIN -- see below
harn provider catalog matrix           # 4. reads the COMPILED-IN table
harn provider catalog support
```

`generate` reads the fragments in this directory and writes `../providers.toml`
and `../capabilities.toml`. Those two files are `include_str!`-ed into the VM,
so the binary that ran `generate` still holds the *previous* table. `matrix`
and `support` render from that compiled-in table, not from the fragments.

Skipping step 3 does not produce an error. `matrix` renders from the old table,
emits output byte-identical to the committed `docs/src/provider-matrix.md`, and
prints `wrote docs/src/provider-matrix.md`. `git status` shows no change, which
reads as "this edit does not affect the matrix" when it actually means "the
matrix was rendered from a table that predates the edit". CI builds from the
committed `capabilities.toml`, renders a matrix that does include the change,
and fails `check-provider-matrix` with `provider capability matrix is stale or
missing`.

Tracked in burin-labs/harn#8062, which proposes making `matrix` and `support`
read the same on-disk fragments `generate` reads so one build suffices.

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

## Choosing a tier

`tier` is a coarse capability class: `small`, `mid`, `frontier`, or
`reasoning`. Rows without one resolve to `tier_defaults.default`.

Judge a row against every model in the catalog, not against its weight class.
Tier is absolute, not size- or price-adjusted: a model does not earn `frontier`
for leading its size class, for being cheap, or for being fast. `strengths`,
`pricing`, and `performance` already carry those claims, and consumers that
want them read those fields.

| Tier | Use for |
| --- | --- |
| `small` | Narrow or short-context routes. Not a general coding driver. |
| `mid` | Genuinely useful general models. The default, and where most open-weight releases and most cheap hosted routes belong. |
| `frontier` | Competitive with the current flagship generation: Opus/Fable/Sonnet-class, GLM-5.x, DeepSeek-V4-Pro, Kimi-K2.7, Qwen3.7-Max. |
| `reasoning` | Frontier-class, and sold as a dedicated reasoning line. |

Capability decisions read this field; commercial ones must not. Cross-family
reviewer selection scores candidates on tier distance, and escalation ladders
read it to decide whether a primary is already strong enough that escalating
would not help. Overstating a tier silently disarms escalation away from that
model, which shows up as a run that never leaves its cheap model rather than as
an error. Price belongs in `pricing`.

### Worked example: gpt-oss-120b

All eight hosted gpt-oss-120b rows carried `frontier` until July 2026. The
model is strong for its class: Artificial Analysis scores it at Intelligence
Index 24, fifth of 62 open-weight 40-150B models. That is the size-adjusted
read this section warns against. Measured against everything tracked, it lands
71st of 390 for intelligence and 78th of 156 for coding, near the median of
tracked coding models, and roughly at parity with o4-mini. Its own rows claim
`strengths = ["speed", "cheap", "tool_use"]`, naming neither coding nor agentic
work. The rows are now `mid`, matching the 20B sibling.
