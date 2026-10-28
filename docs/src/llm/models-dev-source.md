# Update model facts from Models.dev

Update Harn's pinned model facts, then review the generated catalog before shipping.
Run these commands from a Harn source checkout with a CLI built from that checkout.

```sh
harn provider catalog update-source --live
make gen-provider-catalog
make check-provider-catalog check-provider-matrix check-provider-support
```

`update-source` downloads the [Models.dev v2.0 feed](https://models.dev/experimental/v2.0/api.json).
It replaces `crates/harn-vm/src/llm/catalog_sources/models-dev.json` only after validating every selected fact.
The cache records the response's SHA-256 digest and source URL.
Commit the cache and generated projections together.

To use an already downloaded response, replace the first command:

```sh
harn provider catalog update-source --input /tmp/models-dev-v2.json
```

Generation, drift checks, builds, and agent turns use committed data without contacting Models.dev.
Installed runtimes embed the generated snapshot; they don't parse the upstream feed.
Rebuild before testing a changed runtime snapshot.

## Select facts for a route

Declare the exact upstream provider and model in the model's TOML fragment:

```toml
[models."claude-haiku-5-5"]
provider = "anthropic"
models_dev = { provider = "anthropic", model = "claude-haiku-5-5", fields = ["name", "context_window", "open_weight"] }
```

The imported fields must be absent locally, even when the local value would agree.
Supported fields are `name`, `context_window`, `open_weight`, `released`, and `pricing`.
The model still needs Harn's provider, routing, and policy declarations.

Upstream names and route identifiers can differ from Harn's presentation choices.
Keep a field locally when Harn deliberately owns its value; omit that field from `models_dev.fields`.
Record the evidence beside a retained correction, and remove it when the upstream value becomes suitable.
Don't add a copied base value beside an imported field.

Generation rejects invented routes, absent selected facts, unsupported fields, and duplicate ownership.
A refresh that loses a required route fails before replacing the pinned cache.
Review a removal explicitly rather than silently deleting a served route.

## Preserve pricing semantics

The importer accepts simple text rates and generic, five-minute, or one-hour cache writes.
It rejects conditional rates, separate reasoning rates, modality prices, hosted tool fees, and unknown cost fields.
Keep the complete Harn pricing card for those routes until the importer can preserve their meaning.
Never import a base rate while dropping its conditions.

The upstream endpoint is experimental.
Its schema may change independently of Harn; unsupported selected data fails closed.
Leave the committed cache unchanged when an update fails, and fix the adapter or retain the fact locally.
The runtime catalog schema doesn't change merely because the source cache changes.

## Keep runtime policy with Harn

Models.dev supplies selected model facts.
Harn owns request dialects, credentials, defaults, model ladders, and measured tool behavior.
An upstream tool-support flag doesn't prove a working Harn tool loop.
Use the [provider observation workflow](provider-catalog-refresh.md) to investigate provider changes
and verify capability claims.
Its candidate patches are review aids; they don't overwrite the pinned source.

Models.dev uses the [MIT license](https://github.com/anomalyco/models.dev/blob/dev/LICENSE).
`crates/harn-provider-catalog/NOTICE.models-dev` retains its notice with the generated runtime data.
The existing release-notice inventory includes that file in packaged notices.
