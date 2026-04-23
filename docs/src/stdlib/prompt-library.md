# Prompt Library Stdlib

`std/prompt_library` manages reusable prompt fragments and deterministic
hotspot proposals for repeated context prefixes.

```harn,ignore
import "std/prompt_library"

let library = prompt_library_load("prompts.toml")
let prompt_library = prompt_library_api(library)

let system_prefix = prompt_library.inject(
  "rust-repo-conventions-v1",
  {crate: "harn-vm"},
)
```

## Fragment Catalogs

Catalog files are TOML with one or more `[[prompt_fragments]]` entries:

```toml
[[prompt_fragments]]
id = "rust-repo-conventions-v1"
title = "Rust repo conventions"
tags = ["rust", "testing"]
token_budget = 1200
cache_ttl = "5m"
body = "Use nextest for {{crate}} and keep sccache warm."
```

An entry can use `body`, `prompt`, or `text` inline. It can also use `path` to
load a sibling `.harn.prompt` / `.prompt` template relative to the catalog.

Single `.harn.prompt` files can carry TOML front matter:

```harn,ignore
---
id = "ops-prefix"
tags = ["ops"]
cache_ttl = "5m"
---
Operate in {{env}} with low-noise status updates.
```

Fragments default to `cache_control = {type: "ephemeral"}` so hosts can opt
into provider prompt caching. `prompt_library_payload(...)` returns the rendered
text together with that cache metadata; `prompt_library_inject(...)` returns
only text.

## Functions

| Function | Description |
|---|---|
| `prompt_library(fragments?)` | Create an in-memory library |
| `prompt_fragment(id, body, config?)` | Normalize one fragment |
| `prompt_library_load(path_or_paths)` | Load a TOML catalog or front-matter `.harn.prompt` fragment |
| `prompt_library_define(library, fragment)` | Add or replace a fragment by id |
| `prompt_library_list(library, filters?)` | Filter by `id`, `tag`, `tags`, `tenant_id`, `provenance`, or `status` |
| `prompt_library_find(library, id)` | Return one fragment or `nil` |
| `prompt_library_inject(library, id, bindings?)` | Render one fragment to text |
| `prompt_library_payload(library, id, bindings?)` | Render one fragment plus cache metadata |
| `prompt_library_inject_cluster(library, filters?, bindings?)` | Render matching fragments until `max_tokens` is reached |
| `prompt_library_suggest(library, ctx?)` | Rank fragments using query text and tag overlap |
| `prompt_library_api(library)` | Return closure-backed `inject`, `inject_cluster`, `payload`, `suggest`, `list`, and `find` helpers |
| `prompt_library_hotspots(conversations, options?)` | Produce tenant-scoped k-means fragment proposals from recent conversations |
| `prompt_library_review_queue(library, filters?)` | Return pending k-means proposals for review UIs |

## Hotspot Proposals

`prompt_library_hotspots(...)` accepts recent conversations as strings or dicts.
Dict records can include `id`, `tenant_id`, `text` / `prefix` / `prompt`, and an
optional numeric `embedding`. When `embedding` is absent, Harn uses a small
deterministic bag-of-words vector over prompt-setup terms. That keeps the stdlib
worker testable without requiring an embedding provider.

```harn,ignore
let proposals = prompt_library_hotspots(recent_conversations, {
  tenant_id: "tenant-a",
  max_prefix_tokens: 1200,
  min_fraction: 0.8,
  min_shared_tokens: 64,
  daily_invocation_count: 50,
  dollars_per_token: 0.000000003,
  min_monthly_savings_usd: 5.0,
})

let review_library = prompt_library(proposals)
let queue = prompt_library_review_queue(review_library)
```

The function filters to the requested `tenant_id` before clustering. Proposed
fragments are marked `provenance = "kmeans"` and `status = "pending_review"`,
with `members`, `support`, `tokens_saved`, and `monthly_savings_usd` fields for
portal or host review surfaces.
